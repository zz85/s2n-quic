// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Bounded blocking-syscall backend — the production deadlock fix.
//!
//! Storage IO done via `tokio::spawn_blocking` (the `IoPool` pattern of an existing direct-IO crate)
//! deadlocks: each pool spawns blocking threads with no global bound and no backpressure on thread
//! spawn, so under load the pool exhausts and in-flight ops wait on ops that can never be scheduled.
//!
//! This backend fixes that structurally. It owns a **fixed** set of threads — one per execution
//! lane, created once at construction and **never** dynamically spawned. Each lane is the *ordinary
//! combinator future* `Map(rx, execute + complete).drain_budgeted()`, spawned onto a blocking
//! [`Runtime`](blocking::Runtime) (one dedicated OS thread per spawn, driven by
//! [`block_on`](blocking::block_on)). When the lane's channel is momentarily empty the combinator
//! parks the thread via the thread-park waker; a producer's plain `waker.wake()` unparks it, with no
//! special-casing at the send site. The blocking `pread`/`pwrite`/`fsync` runs inline in the map
//! closure (the thread is dedicated, so blocking it is fine), and the finished op is **completed in
//! place** ([`combinator::complete`](crate::fs::combinator::complete)) on that same thread —
//! releasing its credit to its own `Arc<Device>` pool and notifying its submitter. No reaper, no
//! completion bridge.
//!
//! Per-lane channels let the dispatch EDT pick-two balance work across lanes. Crucially, admission is
//! bounded *upstream* by the credit pool (a submitter that can't get credit parks on a waker, never a
//! thread), so a lane's queue can never grow without bound and the hold-and-wait deadlock cannot form.
//!
//! Unlike the bach backend, this runs on real OS threads against real files, so it cannot run under
//! the bach simulated clock — its tests use real threads + temp files.

use bytes::buf::UninitSlice;

use crate::{
    fs::{
        backend::{
            blocking::{JoinOnDrop, Runtime},
            Backend, LaneSetup,
        },
        combinator::{complete, complete_cancelled, DRAIN_BUDGET},
        op::{IoBuf, IoKind, IoOp, IoStatus},
    },
    intrusive::Entry,
    socket::channel::{intrusive::sync as sync_chan, Map, ReceiverExt as _, UnboundedSender},
    sync::Arc,
};
use std::os::fd::RawFd;

/// Run the op's syscall on its fd, stamping `status`.
///
/// **Zero-copy, no memset:** the op operates directly on its own buffer — a buffered read fills the
/// op's pre-sized `BytesMut`'s *uninitialized spare capacity* and sets its length to bytes read (no
/// `resize`/zero-fill), a buffered write `pwrite`s from the op's `Bytes` in place, and a `Direct` op
/// reads into / writes from the caller's page-aligned [`AlignedBuf`] in place. (`O_DIRECT` alignment
/// of the offset is validated at submit time; the buffer pointer/length are aligned by `AlignedBuf`.)
fn execute(op: &mut Entry<IoOp>) {
    let fd = op.fd.as_raw();
    let offset = op.offset;
    let want = op.len as usize;
    let result = match op.kind {
        IoKind::Read => match &mut op.buf {
            IoBuf::Read(buf) => {
                // Buffered read into the pre-sized buffer's uninitialized spare capacity — no memset.
                // `read_exact_into` loops until the full `want` is read or EOF, then we set the
                // logical length to the bytes actually read (a short result means EOF).
                debug_assert!(buf.is_empty(), "read buffer arrives logically empty");
                debug_assert!(
                    buf.capacity() >= want,
                    "read buffer must be pre-sized to op.len"
                );
                // The spare capacity beyond `done` (the buffer's logical len tracks `done`).
                let before_len = buf.len();
                let spare = buf.spare_capacity_mut();
                let spare = UninitSlice::uninit(spare);
                let r = pread_exact(fd, spare, want, offset);
                if let Ok(len) = r {
                    unsafe {
                        buf.set_len(before_len + len);
                    }
                }
                r
            }
            IoBuf::Direct(buf) => {
                // Zero-copy direct read straight into the caller's aligned buffer. `pread_exact`
                // fills the whole length (looping on partial reads), then we set the logical length
                // to the bytes actually read so a short read at EOF exposes no stale/zero tail.
                let spare = UninitSlice::new(buf.as_mut_slice());
                let r = pread_exact(fd, spare, want, offset);
                if let Ok(n) = r {
                    buf.set_len(n);
                }
                r
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "read op without a read buffer",
            )),
        },
        IoKind::Write => match &op.buf {
            IoBuf::Write(data) => pwrite_all(fd, data, offset).map(|()| data.len()),
            // Zero-copy direct write: write straight from the caller's aligned buffer.
            IoBuf::Direct(buf) => pwrite_all(fd, buf.as_slice(), offset).map(|()| buf.len()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "write op without a write buffer",
            )),
        },
        IoKind::Fsync => fsync(fd, false).map(|()| 0),
        IoKind::Fdatasync => fsync(fd, true).map(|()| 0),
        IoKind::Trim => Ok(0), // no-op in v1
    };
    op.status = match result {
        Ok(n) => IoStatus::Done(n),
        Err(e) => IoStatus::Failed(e.kind()),
    };
}

// ── Raw positional syscalls (libc) ──────────────────────────────────────────

/// Read up to `want` bytes into `buf`'s uninitialized spare capacity (no zero-fill), looping on
/// partial reads (`read_exact` semantics — a short `pread` mid-file is not EOF, so we keep reading
/// the tail) until either `want` bytes are read or a read returns 0 (genuine EOF). Sets `buf`'s
/// logical length to the total read. This is the no-memset path: a short read at EOF simply leaves a
/// shorter buffer, never a zero-padded one, and a partial kernel read does not cost a pipeline
/// resubmit.
fn pread_exact(
    fd: RawFd,
    buf: &mut bytes::buf::UninitSlice,
    want: usize,
    offset: u64,
) -> std::io::Result<usize> {
    let want = want.min(buf.len());
    let mut done = 0usize;
    while done < want {
        // The uninitialized destination region for the tail starting at `done`.
        let dst = &mut buf[done..want];
        // SAFETY: `dst` is a valid writable region of `want - done` bytes. The kernel only *writes*
        // it (it never reads the uninitialized bytes), and we return the count so the caller
        // `set_len`s exactly the initialized prefix — no uninitialized byte is ever exposed as
        // readable. `fd` is owned by the caller.
        let ret = unsafe {
            libc::pread(
                fd,
                dst.as_mut_ptr() as *mut libc::c_void,
                dst.len(),
                (offset + done as u64) as libc::off_t,
            )
        };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        let n = ret as usize;
        done += n;
        if n == 0 {
            break; // EOF: fewer than `want` bytes exist.
        }
    }
    Ok(done)
}

/// Write all of `data` at `offset` via repeated `pwrite`. Retries `EINTR`/short writes.
fn pwrite_all(fd: RawFd, data: &[u8], offset: u64) -> std::io::Result<()> {
    let mut written = 0usize;
    while written < data.len() {
        // SAFETY: `data[written..]` is a valid readable slice; `fd` is owned by the caller.
        let ret = unsafe {
            libc::pwrite(
                fd,
                data[written..].as_ptr() as *const libc::c_void,
                data.len() - written,
                (offset + written as u64) as libc::off_t,
            )
        };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if ret == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "pwrite returned 0",
            ));
        }
        written += ret as usize;
    }
    Ok(())
}

fn fsync(fd: RawFd, data_only: bool) -> std::io::Result<()> {
    // `fdatasync` is Linux-only; on other platforms (macOS) fall back to a full `fsync`.
    #[cfg(target_os = "linux")]
    let ret = unsafe {
        if data_only {
            libc::fdatasync(fd)
        } else {
            libc::fsync(fd)
        }
    };
    #[cfg(not(target_os = "linux"))]
    let ret = {
        let _ = data_only;
        // SAFETY: `fd` is owned by the caller for the duration of the call.
        unsafe { libc::fsync(fd) }
    };
    if ret < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ── Backend wiring ──────────────────────────────────────────────────────────

/// A bounded blocking-syscall backend: one dedicated thread per execution lane, each draining its own
/// `sync` submission channel (so the dispatch EDT pick-two routes work across lanes). The thread
/// count is therefore the lane count — fixed at construction, never dynamically spawned, which is
/// what structurally removes the `spawn_blocking` deadlock. The threads come from an injected blocking
/// [`Runtime`]: each lane is the ordinary `Map(rx, execute + complete).drain_budgeted()` combinator
/// future spawned on it, so the backend reuses the standard pipeline machinery rather than a bespoke
/// worker/budget loop, and the `pread`/`pwrite` runs inline on the dedicated thread.
///
/// Direct vs. buffered IO is a per-op property (the [`IoBuf`] variant the submitter chose) and a
/// property of how each file was opened ([`crate::fs::direct::File`] with `direct: true/false`), not
/// a backend-wide mode — so a single backend serves both buffered and `O_DIRECT` files. The op's
/// buffer variant and the file's open flags must agree; a mismatch surfaces as the kernel's `EINVAL`
/// on the syscall (stamped `Failed`), never silent corruption.
pub struct SyscallBackend {
    runtime: Runtime,
}

impl SyscallBackend {
    /// Build a syscall backend over `runtime` (the blocking thread source). One lane future is
    /// spawned on it per execution lane when the scheduler calls [`Backend::spawn_lanes`] (lane count
    /// = `Config::ring_count`).
    pub fn new(runtime: Runtime) -> Self {
        Self { runtime }
    }
}

impl Default for SyscallBackend {
    /// A backend with its own blocking runtime (threads named `s2n-dc-fs-io`).
    fn default() -> Self {
        Self::new(Runtime::new("s2n-dc-fs-io"))
    }
}

impl Backend for SyscallBackend {
    type Lane = LaneSubmit;

    fn spawn_lanes(&self, setup: LaneSetup) -> Vec<LaneSubmit> {
        // One lane future per lane, each owning its lane's submission-channel receiver and the
        // counters it completes ops against. Spawned on the blocking runtime (one dedicated thread
        // each); the resulting join guards are gathered into a shared `Arc<[JoinOnDrop]>` so the
        // threads are joined exactly once — when the last lane handle drops, after the senders have
        // closed the channels and the lane futures have returned.
        let mut senders = Vec::with_capacity(setup.lane_count);
        let mut guards = Vec::with_capacity(setup.lane_count);
        for idx in 0..setup.lane_count {
            let (tx, rx) = sync_chan::new::<IoOp>();
            // The lane IS the standard combinator: pop an op, run its blocking syscall inline, then
            // complete it in place (record on the op's own device counters, release credit to its
            // device pool, notify its submitter).
            let lane = Map::new(rx, move |mut op: Entry<IoOp>| {
                // Skip the syscall entirely if the submitter already dropped its completion receiver:
                // the read/write result would have nowhere to go, so running it is wasted device work.
                // `complete_cancelled` still releases the op's credit (conservation holds).
                if op.is_cancelled() {
                    complete_cancelled(op);
                    return;
                }
                crate::fs::trace::backend_start(&op);
                execute(&mut op);
                crate::fs::trace::backend_done(&op);
                complete(op);
            })
            .drain_budgeted(Some(DRAIN_BUDGET));
            let guard = self.runtime.spawn(idx, lane);
            senders.push(tx);
            guards.push(guard);
        }
        let guards: Arc<[JoinOnDrop]> = guards.into();

        senders
            .into_iter()
            .map(|tx| LaneSubmit {
                tx,
                _guards: guards.clone(),
            })
            .collect()
    }
}

/// The lane's submit handle: pushes each op onto its lane's `sync` channel (waking the lane's thread
/// via the thread-park waker). Holds an `Arc` to the join guards so the threads outlive every lane
/// handle and are joined when the last one drops.
pub struct LaneSubmit {
    tx: sync_chan::Sender<IoOp>,
    _guards: Arc<[JoinOnDrop]>,
}

impl UnboundedSender<Entry<IoOp>> for LaneSubmit {
    fn send(&mut self, op: Entry<IoOp>) -> Result<(), Entry<IoOp>> {
        self.tx.send_entry(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        counter::Registry,
        fs::{
            config::{CostModel, DeviceConfig, OpWeights, PoolMode},
            device::Device,
            direct::{File, Options},
            materialize::materialize_direct,
            scheduler::DeviceRegistry,
        },
        runtime::tokio::Local,
        sched::{CreditConfig, Rate, TierPriority},
    };
    use std::rc::Rc;

    /// A unique temp-file path for a test (no external temp-file crate dependency).
    fn temp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("s2n-dc-fs-{tag}-{}-{nanos}", std::process::id()))
    }

    /// A buffered (non-direct) device sized in bytes; the scheduler paces on byte cost. `Device::new`
    /// floors the grant slice at the per-op ceiling so grants are atomic (indivisible ops).
    fn byte_device(capacity: u64) -> DeviceConfig {
        DeviceConfig {
            pool_mode: PoolMode::Shared(
                CreditConfig::new(capacity)
                    .with_max_single_acquire_uniform(capacity.max(1))
                    .without_refill(),
            ),
            rate: Rate::new(100.0),
            cost_model: CostModel::Bytes,
            op_weights: OpWeights::default(),
            lane_count: 2,
        }
    }

    /// Build a device registry, then register one device lazily — exercising the post-construction
    /// registration path the production setup uses. Returns the registry and the `Arc<Device>` to
    /// submit against (submit is now a method on the device itself).
    fn build(
        spawn: &mut Local,
        registry: &Registry,
        capacity: u64,
    ) -> (DeviceRegistry, Arc<Device>) {
        let reg = DeviceRegistry::new(SyscallBackend::default(), spawn, registry, clock());
        let device = reg.register_device("test", &byte_device(capacity)).unwrap();
        (reg, device)
    }

    /// Run a `!Send` scheduler test body on a tokio current-thread runtime + LocalSet (the real,
    /// non-bach executor M2 needs because the worker pool runs on OS threads). The body receives a
    /// `Local` spawner (which `spawn_local`s onto the `LocalSet`) and a fresh counter `Registry`.
    fn run_local<F, B>(body: B)
    where
        F: std::future::Future<Output = ()> + 'static,
        B: FnOnce(Local, Registry) -> F,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, body(Local::new(0), Registry::default()));
    }

    fn clock() -> crate::busy_poll::clock::Clock {
        crate::busy_poll::clock::Clock::default()
    }

    /// Assert every pool of `device` returned to full capacity (no credit leak).
    fn assert_conserved(device: &Arc<Device>) {
        {
            for pool in device.pools.all() {
                assert_eq!(
                    pool.debug_free_total(),
                    pool.debug_capacity() as i64,
                    "credit leaked on device {:?}",
                    device.label
                );
            }
        }
    }

    /// End-to-end: write data through the scheduler, then read it back and verify the bytes — proving
    /// the bounded pool actually performs real positional IO and routes completions correctly.
    #[test]
    fn write_then_read_roundtrip() {
        let path = temp_path("roundtrip");
        let file = std::sync::Arc::new(
            File::open(
                &path,
                Options {
                    truncate: true,
                    size: 1 << 20,
                    direct: false,
                },
            )
            .unwrap(),
        );
        let fd = crate::fs::op::Fd::new(file.clone());

        run_local(|mut spawn, registry| {
            let path = path.clone();
            async move {
                let (_scheduler, dev) = build(&mut spawn, &registry, 1 << 20);

                let payload =
                    bytes::Bytes::from_static(b"the quick brown fox jumps over the lazy dog");
                let n = dev
                    .write(fd.clone(), 0, payload.clone(), TierPriority::Medium)
                    .await
                    .unwrap();
                assert_eq!(n, payload.len());

                let buf = dev
                    .read(fd.clone(), 0, payload.len() as u32, TierPriority::High)
                    .await
                    .unwrap();
                assert_eq!(
                    &buf[..],
                    &payload[..],
                    "read-back bytes must match what was written"
                );

                // Keep the file handle alive across the IO.
                drop(file);
                let _ = std::fs::remove_file(&path);
            }
        });
    }

    /// Many concurrent submitters far exceed the lane count and the device's credit capacity. The old
    /// `spawn_blocking` IoPool would dynamically spawn threads without bound and could deadlock; here
    /// the credit pool bounds admission and the fixed 2-lane pool drains it all without growing —
    /// every op completes, data is correct, and credit conserves.
    #[test]
    fn high_concurrency_stays_bounded_and_conserves() {
        let path = temp_path("stress");
        let file = std::sync::Arc::new(
            File::open(
                &path,
                Options {
                    truncate: true,
                    size: 1 << 20,
                    direct: false,
                },
            )
            .unwrap(),
        );
        let fd = crate::fs::op::Fd::new(file.clone());

        run_local(|mut spawn, registry| {
            let path = path.clone();
            async move {
                // Tiny capacity (4 KiB) + only 2 lanes, but 32 concurrent writers each doing several
                // ops: admission is credit-bounded, execution is lane-bounded.
                let (_scheduler, dev) = build(&mut spawn, &registry, 4096);

                let completed = Rc::new(std::cell::Cell::new(0usize));
                let mut tasks = Vec::new();
                for w in 0..32u64 {
                    let dev = dev.clone();
                    let fd = fd.clone();
                    let completed = completed.clone();
                    tasks.push(tokio::task::spawn_local(async move {
                        for i in 0..4u64 {
                            let off = (w * 4 + i) * 512;
                            let data = bytes::Bytes::from(vec![(w & 0xff) as u8; 256]);
                            // Each write is 256 bytes; with 4 KiB capacity at most 16 can be admitted
                            // at once, so the rest park on credit (never on a thread).
                            if dev
                                .write(fd.clone(), off, data, TierPriority::Medium)
                                .await
                                .is_ok()
                            {
                                completed.set(completed.get() + 1);
                            }
                        }
                    }));
                }
                for t in tasks {
                    t.await.unwrap();
                }

                assert_eq!(completed.get(), 32 * 4, "every admitted op must complete");
                assert_conserved(&dev);

                drop(file);
                let _ = std::fs::remove_file(&path);
            }
        });
    }

    /// A read past a bad fd surfaces as `Err` rather than hanging, and conserves credit.
    #[test]
    fn bad_fd_read_fails_not_hangs() {
        // An `Fd` wrapping a never-valid descriptor (-1) — the `pread` fails with EBADF.
        struct BadFd;
        impl std::os::fd::AsRawFd for BadFd {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                -1
            }
        }
        run_local(|mut spawn, registry| async move {
            let (_scheduler, dev) = build(&mut spawn, &registry, 1 << 16);

            // fd -1 is never valid: the pread fails with EBADF and must surface as Err promptly.
            let bad = crate::fs::op::Fd::new(Arc::new(BadFd));
            let result = dev.read(bad, 0, 4096, TierPriority::Medium).await;
            assert!(result.is_err(), "read on a bad fd must fail, not hang");

            // Conservation holds on the failure path too.
            assert_conserved(&dev);
        });
    }

    /// Zero-copy direct IO roundtrip: write an aligned buffer in place, read it back into another
    /// aligned buffer in place — no bounce-buffer copy on either path. Uses `O_DIRECT` on Linux
    /// (the `is_tmpfs` guard falls back to buffered if the temp dir is tmpfs, so the test is
    /// portable while still exercising the in-place `IoBuf::Direct` path).
    #[test]
    fn direct_zero_copy_roundtrip() {
        use crate::fs::direct::{AlignedBuf, ALIGNMENT};
        let path = temp_path("direct");
        // Open in direct mode (falls back to buffered on tmpfs / macOS without O_DIRECT semantics).
        let file = std::sync::Arc::new(
            File::open(
                &path,
                Options {
                    truncate: true,
                    size: 1 << 20,
                    direct: true,
                },
            )
            .unwrap(),
        );
        let fd = crate::fs::op::Fd::new(file.clone());

        run_local(|mut spawn, registry| {
            let path = path.clone();
            async move {
                // Capacity in bytes large enough for one block; atomic_grant makes the grant whole.
                let (_scheduler, dev) = build(&mut spawn, &registry, 1 << 20);

                // Build a page-aligned write buffer with a known pattern.
                let mut wbuf = AlignedBuf::new(ALIGNMENT);
                for (i, b) in wbuf.as_mut_slice().iter_mut().enumerate() {
                    *b = (i & 0xff) as u8;
                }
                let (wbuf, n) = dev
                    .write_direct(fd.clone(), 0, wbuf, TierPriority::Medium)
                    .await
                    .expect("direct write");
                assert_eq!(n, ALIGNMENT);
                drop(wbuf);

                // Read it back into a fresh aligned buffer, in place.
                let rbuf = AlignedBuf::new(ALIGNMENT);
                let (rbuf, rn) = dev
                    .read_direct(fd.clone(), 0, rbuf, TierPriority::High)
                    .await
                    .expect("direct read");
                assert_eq!(rn, ALIGNMENT, "direct read returned wrong byte count");
                assert_eq!(rbuf.len(), ALIGNMENT, "direct read buffer len not set to n");
                for (i, b) in rbuf.as_slice().iter().enumerate() {
                    assert_eq!(*b, (i & 0xff) as u8, "direct read byte {i} mismatch");
                }

                // A misaligned offset must be rejected before any IO.
                let bad = AlignedBuf::new(ALIGNMENT);
                let err = dev
                    .read_direct(fd.clone(), 1, bad, TierPriority::Medium)
                    .await;
                assert!(
                    matches!(
                        err.as_ref().map_err(|e| e.kind()),
                        Err(std::io::ErrorKind::InvalidInput)
                    ),
                    "misaligned direct offset must be rejected, got {:?}",
                    err.as_ref().map(|_| ())
                );

                // A misaligned direct LENGTH must also be rejected at submit (not deep-EINVAL).
                let bad_len = AlignedBuf::new(100); // len 100 is not block-aligned
                let err = dev
                    .read_direct(fd.clone(), 0, bad_len, TierPriority::Medium)
                    .await;
                assert!(
                    matches!(
                        err.as_ref().map_err(|e| e.kind()),
                        Err(std::io::ErrorKind::InvalidInput)
                    ),
                    "misaligned direct length must be rejected, got {:?}",
                    err.as_ref().map(|_| ())
                );

                drop(file);
                let _ = std::fs::remove_file(&path);
            }
        });
    }

    /// End-to-end `materialize_direct` over real `O_DIRECT` files: write block-aligned data, then
    /// stream it back through the direct-IO materialize path and verify FIFO order + bytes. Exercises
    /// the zero-copy `AlignedBuf` → `Bytes::from_owner` delivery and the per-device acquire reactor on
    /// the real syscall backend (not just the bach mock).
    #[test]
    fn materialize_direct_streams_in_order() {
        use crate::fs::{
            direct::{AlignedBuf, ALIGNMENT},
            scheduler::BlockRef,
        };
        let path = temp_path("materialize-direct");
        let file = std::sync::Arc::new(
            File::open(
                &path,
                Options {
                    truncate: true,
                    size: 1 << 20,
                    direct: true,
                },
            )
            .unwrap(),
        );
        let fd = crate::fs::op::Fd::new(file.clone());

        // Pre-fill 8 aligned blocks with a per-block pattern (block k -> byte value k). The file is
        // direct, so each write's buffer must be page-aligned too, which a `Vec` isn't.
        let nblocks = 8usize;
        for k in 0..nblocks {
            let mut buf = AlignedBuf::new(ALIGNMENT);
            buf.as_mut_slice().fill(k as u8);
            file.write_at(buf.as_slice(), (k * ALIGNMENT) as u64)
                .unwrap();
        }
        file.sync().unwrap();

        run_local(|mut spawn, registry| {
            let path = path.clone();
            async move {
                let (_scheduler, dev) = build(&mut spawn, &registry, 1 << 20);

                let blocks: Vec<BlockRef> = (0..nblocks)
                    .map(|k| {
                        BlockRef::whole(
                            dev.clone(),
                            fd.clone(),
                            (k * ALIGNMENT) as u64,
                            ALIGNMENT as u32,
                        )
                    })
                    .collect();

                let mut stream = materialize_direct(blocks, TierPriority::High);
                let mut delivered = 0usize;
                while let Some(chunk) = stream.next().await {
                    let bytes = chunk.expect("direct block read failed").copy_to_bytes();
                    assert_eq!(bytes.len(), ALIGNMENT, "block {delivered} wrong len");
                    assert!(
                        bytes.iter().all(|&b| b == delivered as u8),
                        "block {delivered} content/order mismatch"
                    );
                    delivered += 1;
                }
                assert_eq!(
                    delivered, nblocks,
                    "did not deliver all direct blocks in order"
                );
                assert_conserved(&dev);

                drop(file);
                let _ = std::fs::remove_file(&path);
            }
        });
    }

    /// A buffered read past EOF returns only the bytes that exist — the completion's byte count and
    /// the delivered buffer length both reflect the short read, never a zero-padded full length. This
    /// is the read-exact + uninit-buffer path: no memset, `set_len` to the short count.
    #[test]
    fn short_read_at_eof_reports_actual_len() {
        let path = temp_path("eof");
        let file = std::sync::Arc::new(
            File::open(
                &path,
                Options {
                    truncate: true,
                    size: 0,
                    direct: false,
                },
            )
            .unwrap(),
        );
        let fd = crate::fs::op::Fd::new(file.clone());
        // Write exactly 10 bytes, then ask for 4096.
        file.write_at(b"0123456789", 0).unwrap();
        file.sync().unwrap();

        run_local(|mut spawn, registry| {
            let path = path.clone();
            async move {
                let (_scheduler, dev) = build(&mut spawn, &registry, 1 << 20);

                let buf = dev
                    .read(fd.clone(), 0, 4096, TierPriority::Medium)
                    .await
                    .unwrap();
                assert_eq!(
                    buf.len(),
                    10,
                    "short read must report only the bytes that exist"
                );
                assert_eq!(&buf[..], b"0123456789");

                drop(file);
                let _ = std::fs::remove_file(&path);
            }
        });
    }

    /// A device is a single, process-wide, `Send + Sync` object. On ONE multi-threaded runtime, the
    /// registry's own (`!Send`) tasks run on a `LocalSet` while many concurrent submitter tasks are
    /// `tokio::spawn`ed onto the multi-thread pool — which *requires* the submit futures (and the
    /// `Arc<Device>` they capture) to be `Send`. They all submit against one shared `Arc<Device>`, the
    /// model `stream::Client`/`Arc<Endpoint>` uses.
    #[test]
    fn handle_shared_across_threads() {
        let path = temp_path("mt");
        let file = std::sync::Arc::new(
            File::open(
                &path,
                Options {
                    truncate: true,
                    size: 1 << 20,
                    direct: false,
                },
            )
            .unwrap(),
        );
        let fd = crate::fs::op::Fd::new(file.clone());

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();

        let total = local.block_on(&rt, async move {
            // The registry's internal tasks are !Send → the tokio `Local` spawner `spawn_local`s
            // them onto the surrounding `LocalSet`.
            let mut spawn = Local::new(0);
            let registry = Registry::default();
            let reg =
                DeviceRegistry::new(SyscallBackend::default(), &mut spawn, &registry, clock());
            let dev = reg
                .register_device("test", &byte_device(1 << 20).with_lane_count(4))
                .unwrap();

            // Concurrent submitters via `tokio::spawn` (multi-thread pool): this only compiles if the
            // submit future + captured Arc<Device> are Send.
            let threads = 8usize;
            let per_task = 64u64;
            let mut tasks = Vec::new();
            for t in 0..threads {
                let dev = dev.clone();
                let fd = fd.clone();
                tasks.push(tokio::spawn(async move {
                    let mut ok = 0u64;
                    for i in 0..per_task {
                        let off = (t as u64 * per_task + i) * 4096;
                        let data = bytes::Bytes::from(vec![(t & 0xff) as u8; 256]);
                        if dev
                            .write(fd.clone(), off, data, TierPriority::Medium)
                            .await
                            .is_ok()
                        {
                            ok += 1;
                        }
                    }
                    ok
                }));
            }
            let mut total = 0u64;
            for t in tasks {
                total += t.await.unwrap();
            }
            total
        });

        assert_eq!(total, 8 * 64, "all cross-thread submits must complete");
        drop(file);
        let _ = std::fs::remove_file(&path);
    }
}
