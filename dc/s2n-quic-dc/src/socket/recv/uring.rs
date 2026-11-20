// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! io_uring recv backend — one ring per recv socket, multishot `RecvMsg` into provided buffers
//! (Linux only).
//!
//! # Why a separate packet source, not a `recv::Socket`
//!
//! Everything downstream of a received datagram is channel-based: a recv socket reader fills a
//! [`descriptor::Filled`] and hands it to a [`Router`], which hashes it into per-dispatch-worker
//! channels. Nothing downstream cares *how* the bytes arrived. So io_uring is wired as an alternative
//! *source*, not behind the cooperative [`recv::Socket::poll_recv`](crate::socket::recv::Socket) (a
//! kernel-completion model does not fit a poll-from-a-waker interface). A dedicated OS thread owns the
//! socket fd and one [`IoUring`], submits a backlog of receives, blocks for completions, builds the
//! same `Filled` segments, and calls [`Router::on_segment`] **on the ring thread** — the router and
//! the descriptor recycler are `Send` and the dispatch channels are the cross-thread `sync` channels,
//! so the rest of the pipeline is unchanged. The endpoint simply does not spawn the cooperative
//! busy-poll task for a socket it has handed to a ring; that busy-poll loop is the CPU this backend
//! frees.
//!
//! # Multishot recv + provided buffers
//!
//! A single multishot [`RecvMsgMulti`](opcode::RecvMsgMulti) SQE stays armed and yields one CQE per
//! datagram, each naming a kernel-chosen buffer from a registered **buffer ring** (`bgid`). This is
//! the fewest-SQE, lowest-latency shape: no per-packet re-arm in the steady state. The buffers are
//! the recv [`descriptor`]s themselves — each descriptor reserves a fixed
//! [recv prefix](descriptor::RECV_PREFIX_LEN) ahead of its payload, sized so the kernel's
//! `[io_uring_recvmsg_out | name | control | payload]` write lands the payload at the descriptor's
//! `payload_offset`. So a completion is turned into a `Filled` **without an application-side copy**:
//! parse the header, copy the small `sockaddr`/cmsg out, and use the payload where it already sits.
//!
//! # What "no copy" means here (and what it does not)
//!
//! This is *provided-buffer recv*, not kernel zero-copy recv. The kernel still performs the usual
//! `recvmsg` copy out of the socket's `skb` into the buffer we provided; what we avoid is the
//! *second* copy the pipeline would otherwise make to stage those bytes into a `descriptor`. So
//! "no copy" / "zero-copy" throughout this module means **no extra application-side copy** — the
//! payload is delivered in the descriptor the kernel wrote into.
//!
//! True kernel zero-copy recv (`IORING_OP_RECV_ZC` + a registered interface queue, kernel ≥ 6.11)
//! would DMA NIC data straight into pages mapped into our address space, eliminating that remaining
//! in-kernel `skb`→buffer copy — but it needs NIC/driver support (header-data split, RX-queue
//! steering) and a different buffer-refill model, so it is deliberately out of scope here and left
//! as future work.
//!
//! # Buffer lifetime & the bid map
//!
//! The ring owns a `bid -> Unfilled` map (`Vec<Option<Unfilled>>`, one slot per ring entry). Providing
//! a descriptor to the kernel moves it into its `bid` slot and publishes an `io_uring_buf` entry; the
//! descriptor's allocation stays pinned there for the whole kernel operation (the io_uring
//! memory-safety contract). On a CQE the `bid` (from [`buffer_select`](cqueue::buffer_select)) recovers
//! the descriptor, which is finalized into a `Filled` and routed; a fresh descriptor is then allocated
//! from the pool and published into that freed `bid` slot to keep the ring full. Backpressure is pool
//! exhaustion — exactly as on the syscall path.
//!
//! # Dual-wait intake (eventfd)
//!
//! The only out-of-band event is teardown (every lane sender / the keepalive dropped). The ring blocks
//! in `submit_and_wait(1)`; a real CQE wakes it under load, and an eventfd folded into the ring as a
//! multishot [`PollAdd`](opcode::PollAdd) wakes it when idle so teardown is prompt. This mirrors the
//! storage uring backend's dual-wait, minus the producer-intake channel (a recv ring has no submitter
//! side — it pulls from the socket, not from a queue).

use crate::{
    msg::cmsg,
    socket::{
        pool::{
            descriptor::{self, SyncRecycler, Unfilled, RECV_NAME_LEN},
            Pool, SyncReuseRing,
        },
        recv::router::Router,
    },
};
use io_uring::{cqueue, opcode, squeue, types, IoUring};
use std::{
    os::fd::RawFd,
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc,
    },
};

/// Default number of ring entries / provided buffers per recv ring. Sized to hold a large in-flight
/// backlog so the kernel never runs out of buffers between our replenishments under load, while
/// staying small enough that the buffer ring and bid map are a few pages.
pub const DEFAULT_RING_DEPTH: u16 = 1024;

/// Descriptors a ring provides at start: one, the fewest that receive at all.
const INITIAL_FILL: u16 = 1;

/// How many of a ring's bids, from bid 0, hold a provided descriptor. Each `ENOBUFS` completion (a
/// datagram found every provided buffer taken) doubles it, up to the depth. That datagram isn't
/// lost: the kernel picks a buffer before it reads the socket, so it stays queued for the re-arm.
struct FillWindow {
    filled: u16,
    depth: u16,
}

impl FillWindow {
    fn new(depth: u16) -> Self {
        Self {
            filled: INITIAL_FILL.min(depth),
            depth,
        }
    }

    fn bids(&self) -> core::ops::Range<u16> {
        0..self.filled
    }

    /// On `ENOBUFS`, doubles the window and returns the bids it newly holds; else returns none.
    fn on_recv_without_buffer(&mut self, res: i32) -> core::ops::Range<u16> {
        let old = self.filled;
        if res == -libc::ENOBUFS {
            self.filled = old.saturating_mul(2).min(self.depth);
        }
        old..self.filled
    }
}

/// Reserved `user_data` for the armed eventfd [`PollAdd`](opcode::PollAdd). Distinct from the single
/// recv op's user_data ([`RECV_USER_DATA`]).
const EVENTFD_TOKEN: u64 = u64::MAX;

/// `user_data` for the multishot recv SQE. There is exactly one armed recv op per ring, so a single
/// constant token suffices (the buffer is identified by the CQE's `bid`, not by `user_data`).
const RECV_USER_DATA: u64 = 1;

/// Probe whether io_uring can be set up in the current environment, returning the underlying
/// `io_uring_setup(2)` error if not, so the caller can fall back to the syscall recv path and log
/// *why* (ENOSYS = kernel < 5.1 / built without it; EPERM = `kernel.io_uring_disabled` sysctl or a
/// seccomp filter; ENOMEM = `RLIMIT_MEMLOCK` too low). Creates and immediately drops a minimal ring.
pub fn probe() -> std::io::Result<()> {
    IoUring::new(8).map(drop)
}

/// The system page size, queried once via `sysconf(_SC_PAGESIZE)`. The buffer ring base must be
/// aligned to this (not a hard-coded 4096) for `IORING_REGISTER_PBUF_RING` to succeed on kernels
/// built with larger pages (commonly 16 KiB / 64 KiB on arm64).
fn system_page_size() -> usize {
    // SAFETY: `sysconf` is a plain libc query with no preconditions.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    // Fall back to 4096 if the query fails (returns -1); 4096 is the minimum on every supported arch.
    if v <= 0 {
        4096
    } else {
        v as usize
    }
}

// ── eventfd wake source (teardown) ─────────────────────────────────────────

/// An owned `eventfd(2)` used only to break the ring's idle `submit_and_wait` at teardown.
struct EventFd {
    fd: RawFd,
}

impl EventFd {
    fn new() -> std::io::Result<Self> {
        // SAFETY: `eventfd` is a plain syscall; the flags are valid.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    fn signal(&self) {
        let val: u64 = 1;
        // SAFETY: `&val` is a valid 8-byte source; `self.fd` is open for the call.
        let _ = unsafe { libc::write(self.fd, &val as *const u64 as *const libc::c_void, 8) };
    }

    fn drain(&self) {
        let mut buf = [0u8; 8];
        // SAFETY: `buf` is a valid 8-byte destination; a non-blocking read returns 8 or EAGAIN.
        let _ = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, 8) };
    }
}

impl Drop for EventFd {
    fn drop(&mut self) {
        // SAFETY: we own `fd` and it is not used after close.
        unsafe { libc::close(self.fd) };
    }
}

/// Shared shutdown signal for a recv ring: the handle sets `closed` and writes the eventfd to break
/// the ring's wait. The ring loop observes `closed` and exits.
struct Shutdown {
    closed: AtomicBool,
    efd: EventFd,
}

impl Shutdown {
    fn signal_close(&self) {
        self.closed.store(true, Ordering::Release);
        self.efd.signal();
    }
}

// ── Buffer ring ─────────────────────────────────────────────────────────────

/// A registered io_uring **buffer ring** (`IORING_REGISTER_PBUF_RING`): a page-aligned array of
/// [`io_uring_buf`](types::BufRingEntry) entries shared with the kernel. We publish a descriptor's
/// recv region into a slot and advance the shared tail; the kernel consumes from the head and reports
/// the chosen buffer's `bid` in each recv CQE.
struct BufRing {
    /// Page-aligned backing allocation of `entries` × 16-byte `io_uring_buf` structs.
    ring: *mut types::BufRingEntry,
    /// Number of entries — a power of two, so `mask = entries - 1`.
    entries: u16,
    mask: u16,
    /// Buffer-group id this ring is registered under.
    bgid: u16,
    /// Our private cache of the shared tail (the kernel reads the tail from the ring's first entry's
    /// `resv` field; we keep a copy to compute the next slot without reading shared memory).
    local_tail: u16,
}

// SAFETY: `BufRing` is constructed on the calling thread (in `spawn`) and then moved onto the
// dedicated ring thread, which is its sole accessor thereafter. The raw `ring` pointer makes it
// `!Send` by default; the move is sound because no other thread retains a reference (the kernel reads
// it via the registration, not via Rust aliasing).
unsafe impl Send for BufRing {}

impl BufRing {
    /// Allocate and register a buffer ring of `entries` (rounded up to a power of two) under `bgid`.
    ///
    /// # Safety
    ///
    /// The registration pins `ring` for the kernel; `BufRing` must outlive the registration (enforced
    /// by unregistering in [`Drop`] before the allocation is freed) and the ring fd must stay open.
    unsafe fn register(ring_fd: &IoUring, entries: u16, bgid: u16) -> std::io::Result<Self> {
        let entries = entries.next_power_of_two().max(2);
        let layout = Self::layout(entries);
        // SAFETY: non-zero layout; we check for null below.
        let ring = std::alloc::alloc_zeroed(layout) as *mut types::BufRingEntry;
        if ring.is_null() {
            return Err(std::io::Error::from(std::io::ErrorKind::OutOfMemory));
        }
        // SAFETY: `ring` is a valid, page-aligned allocation of `entries` 16-byte structs that stays
        // alive (and registered) until `Drop` unregisters it.
        ring_fd
            .submitter()
            .register_buf_ring_with_flags(ring as u64, entries, bgid, 0)?;
        Ok(Self {
            ring,
            entries,
            mask: entries - 1,
            bgid,
            local_tail: 0,
        })
    }

    fn layout(entries: u16) -> std::alloc::Layout {
        // `IORING_REGISTER_PBUF_RING` requires the ring base to be aligned to the *system* page size,
        // which is not always 4096 (arm64 kernels are commonly built with 16 KiB or 64 KiB pages).
        // Query it at runtime rather than hard-coding 4096, which would make registration fail with
        // EINVAL on those kernels.
        let page = system_page_size();
        std::alloc::Layout::from_size_align(entries as usize * 16, page)
            .expect("valid buf ring layout")
    }

    /// Publish one buffer (`addr`/`len`, identified by `bid`) into the next tail slot. Does **not**
    /// advance the shared tail — call [`commit`](Self::commit) after publishing a batch.
    ///
    /// # Safety
    ///
    /// `addr`/`len` must reference a buffer that stays valid and exclusively owned by the kernel until
    /// its recv CQE is reaped (the descriptor pinned in the bid map).
    unsafe fn publish(&mut self, addr: *mut u8, len: u32, bid: u16) {
        let idx = (self.local_tail & self.mask) as isize;
        // SAFETY: `idx` is within `[0, entries)`; the entry is part of our allocation.
        let entry = &mut *self.ring.offset(idx);
        entry.set_addr(addr as u64);
        entry.set_len(len);
        entry.set_bid(bid);
        self.local_tail = self.local_tail.wrapping_add(1);
    }

    /// Publish the advanced tail to the kernel (a single `Release` store of the shared tail field).
    fn commit(&self) {
        // SAFETY: `ring` points at a valid first entry whose `resv` field is the shared tail.
        let tail_ptr = unsafe { types::BufRingEntry::tail(self.ring) as *mut u16 };
        // SAFETY: `tail_ptr` is a valid, aligned u16 in the shared ring; Release pairs with the
        // kernel's Acquire load of the tail.
        unsafe { (*(tail_ptr as *const AtomicU16)).store(self.local_tail, Ordering::Release) };
    }
}

impl Drop for BufRing {
    fn drop(&mut self) {
        // The ring fd is closed by the time the loop drops its `IoUring`; unregister-by-bgid is a
        // best-effort cleanup while it is still open (done in `ring_loop` before this drops). Free the
        // backing allocation. SAFETY: `ring` came from `alloc_zeroed` with this exact layout and is no
        // longer referenced by the kernel (the ring fd is gone, so the registration is void).
        unsafe { std::alloc::dealloc(self.ring as *mut u8, Self::layout(self.entries)) };
    }
}

// ── Ring loop ─────────────────────────────────────────────────────────────

/// Build the fixed `msghdr` the multishot recv uses: only `msg_namelen` / `msg_controllen` are read
/// by the kernel for a provided-buffer multishot recv, and they must match the reserved prefix
/// regions so the payload lands at `payload_offset`.
fn recv_msghdr() -> libc::msghdr {
    // SAFETY: `msghdr` is plain-old-data; zeroing then setting the two length fields is the documented
    // way to drive a multishot recvmsg.
    let mut msg: libc::msghdr = unsafe { core::mem::zeroed() };
    msg.msg_namelen = RECV_NAME_LEN as u32;
    msg.msg_controllen = descriptor::RECV_CONTROL_LEN as _;
    msg
}

/// Arm the multishot recv SQE: receive from `fd` into a buffer drawn from buffer group `bgid`.
fn build_recv_sqe(fd: RawFd, msg: *const libc::msghdr, bgid: u16) -> squeue::Entry {
    opcode::RecvMsgMulti::new(types::Fd(fd), msg, bgid)
        .build()
        .user_data(RECV_USER_DATA)
}

/// Arm a multishot poll on the eventfd for `POLLIN`, tagged with [`EVENTFD_TOKEN`].
fn build_eventfd_poll(efd: RawFd) -> squeue::Entry {
    opcode::PollAdd::new(types::Fd(efd), libc::POLLIN as u32)
        .multi(true)
        .build()
        .user_data(EVENTFD_TOKEN)
}

/// The recv ring thread. Takes the already-created ring + registered buffer ring (built fallibly in
/// [`spawn`] before this thread starts, so registration failures fall back cleanly), owns the socket
/// fd and the bid→descriptor map, keeps the ring full of provided buffers, reaps recv completions into
/// `Filled` segments, and routes them. Returns when `shutdown.closed` is observed.
fn ring_loop<R: Router>(
    mut ring: IoUring,
    mut buf_ring: BufRing,
    fd: RawFd,
    pool: Pool,
    mut reuse: SyncReuseRing,
    mut router: R,
    shutdown: Arc<Shutdown>,
) {
    let depth = buf_ring.entries;
    let bgid = buf_ring.bgid;
    let mut window = FillWindow::new(depth);

    // bid -> the descriptor currently provided to the kernel in that buffer slot. A descriptor parked
    // here is owned by the kernel until its recv CQE arrives.
    let mut bid_map: Vec<Option<Unfilled<SyncRecycler>>> = (0..depth).map(|_| None).collect();

    // Fixed msghdr referenced by the (single) multishot recv SQE for the whole loop.
    let msg = recv_msghdr();

    // Fill the window: allocate a descriptor per bid, publish its recv region, park it in bid_map.
    for bid in window.bids() {
        let Some(unfilled) = reuse.alloc_or_reuse(&pool) else {
            // Pool exhausted at startup — provide what we have; the loop replenishes as CQEs free bids.
            break;
        };
        let (addr, len) = unfilled.recv_prefix_region();
        // SAFETY: the descriptor is parked in `bid_map[bid]` for the kernel operation's duration, so
        // `addr`/`len` stay valid and exclusively owned by the kernel until the CQE.
        unsafe { buf_ring.publish(addr, len, bid) };
        bid_map[bid as usize] = Some(unfilled);
    }
    buf_ring.commit();

    // Arm the eventfd poll (idle wake / teardown) and the single multishot recv. On a freshly created
    // ring the SQ has `depth` slots free, so pushing two entries cannot fail; assert it so a future
    // change that shrinks the SQ can't silently leave the ring unarmed (no wake source / no recv).
    {
        let mut sq = ring.submission();
        // SAFETY: the eventfd outlives the ring (owned via `shutdown`); the SQE references only its fd.
        unsafe { sq.push(&build_eventfd_poll(shutdown.efd.fd)) }
            .expect("SQ has room for the eventfd poll on a fresh ring");
        // SAFETY: `msg` lives on this stack frame for the whole loop; the kernel reads only its length
        // fields and the provided buffers (pinned in `bid_map`).
        unsafe { sq.push(&build_recv_sqe(fd, &msg, bgid)) }
            .expect("SQ has room for the multishot recv on a fresh ring");
    }

    let mut recv_armed = true;
    // Scratch list of bids freed this reap pass, replenished after routing.
    let mut to_replenish: Vec<u16> = Vec::with_capacity(depth as usize);

    loop {
        if shutdown.closed.load(Ordering::Acquire) {
            break;
        }

        // Re-arm the multishot recv if a prior CQE reported it is no longer armed (kernel ran out of
        // buffers, or a transient error terminated the multishot). Done before waiting so a recv is
        // always outstanding when buffers are available.
        if !recv_armed {
            let mut sq = ring.submission();
            // SAFETY: same invariants as the initial arm.
            if unsafe { sq.push(&build_recv_sqe(fd, &msg, bgid)) }.is_ok() {
                recv_armed = true;
            }
        }

        match ring.submit_and_wait(1) {
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::warn!(%err, "io_uring recv submit_and_wait error");
            }
        }

        to_replenish.clear();
        let mut saw_eventfd = false;
        {
            let mut cq = ring.completion();
            cq.sync();
            for cqe in &mut cq {
                let token = cqe.user_data();
                if token == EVENTFD_TOKEN {
                    saw_eventfd = true;
                    continue;
                }
                debug_assert_eq!(token, RECV_USER_DATA);

                // The multishot recv is no longer armed once the kernel clears F_MORE — re-arm above.
                if !cqueue::more(cqe.flags()) {
                    recv_armed = false;
                }

                let res = cqe.result();
                let Some(bid) = cqueue::buffer_select(cqe.flags()) else {
                    // No buffer attached. A negative result with no buffer is a recv-level error
                    // (e.g. -ENOBUFS when the ring was momentarily empty): nothing to free, the
                    // re-arm + replenish below recovers. Non-negative with no buffer should not occur.
                    if res < 0 {
                        tracing::trace!(errno = -res, "recv cqe error without buffer");
                    }
                    // -ENOBUFS grows the window; its new bids are provided with the freed ones.
                    to_replenish.extend(window.on_recv_without_buffer(res));
                    continue;
                };

                // Reclaim the descriptor parked in this bid slot regardless of outcome.
                let Some(unfilled) = bid_map[bid as usize].take() else {
                    debug_assert!(false, "recv CQE for an unprovided bid {bid}");
                    continue;
                };

                if res < 0 {
                    // Error completion: drop the descriptor (recycles) and replenish the bid.
                    tracing::trace!(errno = -res, bid, "recv error completion");
                    drop(unfilled);
                    to_replenish.push(bid);
                    continue;
                }

                // Successful completion: the kernel wrote `[recvmsg_out | name | control | payload]`
                // into the descriptor's recv region. Finalize into a `Filled` (no application-side
                // copy — the payload is used in place) and route.
                match finalize(unfilled, res as usize) {
                    Some(segments) => {
                        for segment in segments {
                            router.on_segment(segment);
                        }
                    }
                    None => {
                        // Parse/length anomaly — descriptor already dropped in `finalize`.
                    }
                }
                to_replenish.push(bid);
            }
        }
        if saw_eventfd {
            shutdown.efd.drain();
        }

        // Replenish freed bids with fresh descriptors so the ring stays full. Pool exhaustion leaves a
        // bid empty (backpressure); a later pass retries it once descriptors recycle.
        if !to_replenish.is_empty() {
            for &bid in &to_replenish {
                if let Some(unfilled) = reuse.alloc_or_reuse(&pool) {
                    let (addr, len) = unfilled.recv_prefix_region();
                    // SAFETY: parked in `bid_map[bid]` for the kernel operation's duration.
                    unsafe { buf_ring.publish(addr, len, bid) };
                    bid_map[bid as usize] = Some(unfilled);
                }
            }
            buf_ring.commit();
        }
    }

    // Teardown: unregister the buffer ring while the ring fd is still open, then drop everything. Any
    // descriptors still parked in the bid map recycle/dealloc on drop.
    let _ = ring.submitter().unregister_buf_ring(buf_ring.bgid);
    drop(bid_map);
}

/// Turn a successful recv completion into `Filled` segments without an application-side payload copy
/// (the payload is used in place; see the module docs on what "no copy" does and does not mean).
/// `total` is the CQE byte count
/// — the length of the `[recvmsg_out | name | control | payload]` the kernel wrote into the recv
/// region. Returns `None` (dropping the descriptor) on a parse/length anomaly.
fn finalize(
    unfilled: Unfilled<SyncRecycler>,
    total: usize,
) -> Option<descriptor::SegmentsIter<SyncRecycler>> {
    use io_uring::types::RecvMsgOut;

    let (region_ptr, region_len) = unfilled.recv_prefix_region();
    let region_len = region_len as usize;
    if total > region_len {
        tracing::trace!(total, region_len, "recv length exceeds region");
        return None;
    }
    // SAFETY: the kernel wrote `total` bytes into the recv region starting at `region_ptr`; we read
    // only those bytes to parse the recvmsg_out header + name + control.
    let buffer = unsafe { core::slice::from_raw_parts(region_ptr, total) };
    let msg = recv_msghdr();
    let out = match RecvMsgOut::parse(buffer, &msg) {
        Ok(out) => out,
        Err(()) => {
            tracing::trace!(total, "failed to parse recvmsg_out");
            return None;
        }
    };

    // Extract the small fields we need before consuming the descriptor: peer addr (from name), ECN +
    // GRO segment_len (from control). Copy these out — the payload stays in place.
    let payload_len = out.payload_data().len();
    let name = out.name_data();
    let remote = if name.len() >= core::mem::size_of::<libc::sockaddr_in>() {
        // SAFETY: `name` holds at least a `sockaddr_in`; `addr::read` validates the family/len.
        Some(unsafe { crate::msg::addr::read(name.as_ptr(), name.len()) })
    } else {
        None
    };
    let mut cmsg_recv = cmsg::Receiver::default();
    parse_control(out.control_data(), &mut cmsg_recv);
    let ecn = cmsg_recv.ecn();
    let segment_len = cmsg_recv.segment_len();

    // Publish the payload in place via `fill_with`: the closure does not touch the payload iov (the
    // kernel already wrote it) — it only stamps the addr/ecn and returns the received length, so the
    // resulting `Filled` spans exactly the received payload at `payload_offset` — no application-side
    // copy.
    let result = unfilled.fill_with(|addr, cmsg_out, _iov| {
        if let Some(remote) = remote {
            addr.set(remote);
        }
        cmsg_out.set_ecn(ecn);
        if segment_len > 0 {
            cmsg_out.set_segment_len(segment_len);
        }
        <Result<usize, core::convert::Infallible>>::Ok(payload_len)
    });
    match result {
        Ok(segments) => Some(segments.into_iter()),
        Err(_) => None,
    }
}

/// Decode the kernel-written control region into a [`cmsg::Receiver`] (ECN + GRO segment size) by
/// driving the existing cmsg decoder over a synthesized `msghdr` pointing at the control bytes.
fn parse_control(control: &[u8], recv: &mut cmsg::Receiver) {
    if control.is_empty() {
        return;
    }
    // SAFETY: plain-old-data msghdr; we point its control fields at the kernel-written control bytes
    // and let `cmsg::Receiver::with_msg` decode them. `with_msg` only reads.
    let mut msg: libc::msghdr = unsafe { core::mem::zeroed() };
    msg.msg_control = control.as_ptr() as *mut libc::c_void;
    msg.msg_controllen = control.len() as _;
    recv.with_msg(&msg);
}

// ── Handle ──────────────────────────────────────────────────────────────────

/// A running recv ring's lifetime guard: signals shutdown and joins the ring thread on drop.
pub struct RecvRing {
    shutdown: Arc<Shutdown>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RecvRing {
    fn drop(&mut self) {
        self.shutdown.signal_close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Why a recv ring could not be spawned.
pub enum SpawnError<S, R> {
    /// Setup failed *before* the socket and router were committed to the ring thread, so both are
    /// handed back for a clean fall-back to the syscall recv path. Carries the underlying error.
    Recoverable(std::io::Error, S, R),
    /// The ring thread could not be spawned (resource exhaustion) *after* the socket and router were
    /// moved into its closure, so they are gone and this socket cannot be serviced by either path.
    /// Carries the error. Effectively unreachable after a successful [`probe`].
    Fatal(std::io::Error),
}

/// Spawn a dedicated thread that **owns** `socket`, runs a multishot-recv ring driven by the socket's
/// raw fd, and routes received datagrams through `router`. The ring thread holds the socket for its
/// whole lifetime (keeping the fd open), so the returned [`RecvRing`] need not — dropping the
/// `RecvRing` signals the thread to stop and joins it, after which the socket is dropped on that
/// thread. `socket` need only be `Send` (the ring thread is its sole accessor).
pub fn spawn<S, R>(
    idx: usize,
    socket: S,
    depth: u16,
    pool: Pool,
    reuse: SyncReuseRing,
    router: R,
) -> Result<RecvRing, SpawnError<S, R>>
where
    S: crate::socket::recv::BindOnWorker + Send + 'static,
    R: Router + Send + 'static,
{
    // io_uring drives the socket by its raw fd on the ring thread and never polls it via `recv::Socket`
    // (it only owns the socket to keep the fd open), so an unbound recv socket is fine here — take the
    // fd from `BindOnWorker` and skip readiness binding entirely (io_uring needs no tokio `AsyncFd`).
    let Some(fd) = crate::socket::recv::BindOnWorker::raw_fd(&socket) else {
        // No real OS fd — io_uring cannot drive it. Hand both back for the syscall path.
        return Err(SpawnError::Recoverable(
            std::io::Error::from(std::io::ErrorKind::Unsupported),
            socket,
            router,
        ));
    };
    let depth = depth.next_power_of_two().max(2);
    let bgid = 0;

    // Do ALL fallible setup here, on the calling thread, BEFORE `socket`/`router` are committed to the
    // ring thread — so any failure (eventfd, ring creation, or buffer-ring registration) hands them
    // back as `Recoverable` for a clean fall-back to the syscall recv path. This matters because
    // `probe()` only proves `io_uring_setup` works (kernel >= 5.1); provided-buffer rings need >= 5.19
    // and can also fail on `RLIMIT_MEMLOCK`, so registration can fail even after a successful probe.
    let efd = match EventFd::new() {
        Ok(efd) => efd,
        Err(err) => return Err(SpawnError::Recoverable(err, socket, router)),
    };
    let ring = match IoUring::new(depth as u32) {
        Ok(r) => r,
        Err(err) => return Err(SpawnError::Recoverable(err, socket, router)),
    };
    // SAFETY: the buffer ring is owned by the loop (moved in below), unregistered in `ring_loop`'s
    // teardown before the ring fd drops, and outlives every published buffer.
    let buf_ring = match unsafe { BufRing::register(&ring, depth, bgid) } {
        Ok(b) => b,
        Err(err) => return Err(SpawnError::Recoverable(err, socket, router)),
    };

    let shutdown = Arc::new(Shutdown {
        closed: AtomicBool::new(false),
        efd,
    });
    let ring_shutdown = shutdown.clone();
    let join = std::thread::Builder::new()
        .name(format!("s2n-dc-recv-uring-{idx}"))
        .spawn(move || {
            // Own the socket for the thread's lifetime so its fd stays open while the ring drives it;
            // it is dropped here when the loop returns (after the fd is no longer referenced by any
            // in-flight SQE — the loop tears the ring down before returning).
            let _socket = socket;
            ring_loop(ring, buf_ring, fd, pool, reuse, router, ring_shutdown);
        });
    match join {
        Ok(join) => Ok(RecvRing {
            shutdown,
            join: Some(join),
        }),
        Err(err) => Err(SpawnError::Fatal(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::{
        pool::{descriptor::Filled, Pool, SyncReuseRing},
        BusyPoll,
    };
    use std::{
        net::UdpSocket,
        sync::{Arc, Mutex},
        time::Duration,
    };

    /// Captured segments: each entry is `(payload bytes, source port)`.
    type CapturedSegments = Arc<Mutex<Vec<(Vec<u8>, u16)>>>;

    /// A `Router` that captures each received segment's payload + peer address into a shared vec, so a
    /// test can assert exactly what the ring delivered. It overrides `on_segment` to bypass packet
    /// decoding (the test sends arbitrary bytes, not real dc packets).
    #[derive(Clone)]
    struct CaptureRouter {
        received: CapturedSegments,
    }

    impl Router for CaptureRouter {
        fn is_open(&self) -> bool {
            true
        }

        fn on_segment(&mut self, segment: Filled) {
            // Capture the payload bytes and the source port (enough to assert correct delivery +
            // address parsing without depending on the canonical address type's private fields).
            let port = segment.remote_address().get().port();
            let payload = segment.payload().to_vec();
            self.received.lock().unwrap().push((payload, port));
        }
    }

    /// End-to-end through a real io_uring multishot recv ring: bind a loopback UDP socket, drive it
    /// with a ring, send datagrams from another socket, and verify each arrives with the correct
    /// payload and source address. Exercises the shared descriptor layout, the buffer ring, and
    /// `RecvMsgOut` parsing on real hardware. Skipped (passes trivially) if io_uring is unavailable.
    #[test]
    fn uring_recv_roundtrip() {
        if probe().is_err() {
            eprintln!("io_uring unavailable; skipping uring_recv_roundtrip");
            return;
        }

        let recv_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        recv_sock.set_nonblocking(true).unwrap();
        let recv_addr = recv_sock.local_addr().unwrap();
        let socket = BusyPoll(recv_sock);

        let received = Arc::new(Mutex::new(Vec::new()));
        let router = CaptureRouter {
            received: received.clone(),
        };

        let pool = Pool::new(u16::MAX);
        let reuse = SyncReuseRing::new();
        let ring = spawn(0, socket, 64, pool, reuse, router)
            .unwrap_or_else(|_| panic!("recv ring spawn must succeed when io_uring is available"));

        // Give the ring thread a moment to register the buffer ring and arm the multishot recv.
        std::thread::sleep(Duration::from_millis(50));

        let send_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let send_addr = send_sock.local_addr().unwrap();
        let payloads: &[&[u8]] = &[b"hello uring", b"second datagram", &[0xab; 1200]];
        for p in payloads {
            send_sock.send_to(p, recv_addr).unwrap();
        }

        // Poll for delivery (the ring runs on its own thread).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if received.lock().unwrap().len() >= payloads.len() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {} datagrams; got {}",
                payloads.len(),
                received.lock().unwrap().len()
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        drop(ring); // stop + join the ring thread

        let got = received.lock().unwrap();
        assert_eq!(got.len(), payloads.len(), "datagram count");
        for (i, expected) in payloads.iter().enumerate() {
            assert_eq!(&got[i].0, expected, "payload {i} bytes");
            assert_eq!(got[i].1, send_addr.port(), "payload {i} source port");
        }
    }

    /// The window starts at `INITIAL_FILL` and each `ENOBUFS` doubles it, up to the depth.
    #[test]
    fn fill_window_doubles_on_enobufs() {
        let mut window = FillWindow::new(DEFAULT_RING_DEPTH);
        assert_eq!(window.bids(), 0..INITIAL_FILL);

        let mut filled = INITIAL_FILL;
        while filled < DEFAULT_RING_DEPTH {
            let added = window.on_recv_without_buffer(-libc::ENOBUFS);
            assert_eq!(added, filled..filled * 2);
            filled *= 2;
            assert_eq!(window.bids(), 0..filled);
        }

        let added = window.on_recv_without_buffer(-libc::ENOBUFS);
        assert!(added.is_empty());
        assert_eq!(window.bids(), 0..DEFAULT_RING_DEPTH);
    }

    /// Another error without a buffer leaves the window as it is.
    #[test]
    fn fill_window_holds_on_other_errors() {
        let mut window = FillWindow::new(DEFAULT_RING_DEPTH);
        let added = window.on_recv_without_buffer(-libc::ECONNREFUSED);
        assert!(added.is_empty());
        assert_eq!(window.bids(), 0..INITIAL_FILL);
    }

    /// A burst far past the first window all arrives, in order. Skipped if io_uring is unavailable.
    #[test]
    fn uring_burst_past_window() {
        if probe().is_err() {
            eprintln!("io_uring unavailable; skipping uring_burst_past_window");
            return;
        }

        let recv_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        recv_sock.set_nonblocking(true).unwrap();
        let recv_addr = recv_sock.local_addr().unwrap();
        let socket = BusyPoll(recv_sock);

        let received = Arc::new(Mutex::new(Vec::new()));
        let router = CaptureRouter {
            received: received.clone(),
        };

        let pool = Pool::new(u16::MAX);
        let reuse = SyncReuseRing::new();
        let ring = spawn(0, socket, 64, pool, reuse, router)
            .unwrap_or_else(|_| panic!("recv ring spawn must succeed when io_uring is available"));

        // Give the ring thread a moment to register the buffer ring and arm the multishot recv.
        std::thread::sleep(Duration::from_millis(50));

        let send_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let count = 64u8;
        for i in 0..count {
            send_sock.send_to(&[i; 100], recv_addr).unwrap();
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while received.lock().unwrap().len() < usize::from(count) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {count} datagrams; got {}",
                received.lock().unwrap().len()
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        drop(ring); // stop + join the ring thread

        let got = received.lock().unwrap();
        let marks: Vec<u8> = got.iter().map(|(payload, _)| payload[0]).collect();
        let expected: Vec<u8> = (0..count).collect();
        assert_eq!(marks, expected, "datagrams in order");
    }
}
