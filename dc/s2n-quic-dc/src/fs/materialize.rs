// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Ordered streaming reads — spray + reassemble over a single completion queue.
//!
//! [`MaterializeStream`] reads a logical object whose blocks scatter across devices and delivers the
//! bytes in **FIFO order** even though the block reads complete out of order. It replaces the
//! per-stream `futures::stream::iter(blocks).map(read).buffered(N)` island each storage-service
//! materialization spins up today (one `spawn_blocking` storm per stream, no coordination — the
//! deadlock and the lost fairness).
//!
//! Each source item is a [`Block`]: a [`Read`](Block::Read) (a `BlockRef` fetched from a device,
//! credit-gated) or a [`Resident`](Block::Resident) (already-in-memory [`ByteVec`] spliced into the
//! stream in order, **bypassing** `prepare`/credit/device/backend). Storage-service objects are a mix of
//! on-disk extents and cached/just-written data, so a stream interleaves both and the consumer sees
//! one seamless ordered byte stream.
//!
//! # Shape
//!
//! Every block is sprayed via the scheduler's submit path against **one** shared completion channel,
//! tagged with its block index as `user_data`; the stream drains that single channel with one atomic
//! [`poll_swap`](crate::socket::channel::intrusive::datagram_completion::Receiver::poll_swap) and
//! reassembles by index in a `VecDeque` reorder window offset by the deliver cursor. There is no
//! per-read `Box::pin` future and no "poll every in-flight future" window — it is a single
//! `poll_next` state machine, the same poll-`Inner` shape `stream::reader`/`stream::writer` use.
//!
//! # Cross-device head-of-line blocking
//!
//! Credit pools are **per device**, and a materialize stream's blocks scatter across devices. If the
//! stream acquired credit for one block at a time, a block whose device is saturated would block
//! submitting a *later* block whose device has free credit — head-of-line blocking that wastes the
//! idle device's IOPS. So the stream holds **one credit-acquire slot per device** ([`SubmitterAlloc`])
//! and drives them concurrently from the single `poll_next`: a device whose slot is parked on credit
//! does not stop another device's block from being submitted. Submission is therefore *out of order
//! across devices*; delivery stays FIFO via the reorder window.
//!
//! # In-flight bound
//!
//! Two distinct limits apply, because credit is released at **completion**, not at delivery: a
//! completed-but-undelivered block sits in the window holding its read buffer with its credit already
//! freed. So the per-device credit pool bounds *submitted-but-not-completed* work, and a separate
//! [`MAX_RESIDENT_BLOCKS`] cap on the window length bounds *submitted-but-not-delivered* work — the
//! resident-buffer ceiling that protects against a slow consumer letting a fast device pile up
//! completed buffers. The cap is an internal constant, not a caller knob: the credit pool governs the
//! healthy case; the cap only bites when the consumer stalls.

use crate::{
    byte_vec::ByteVec,
    credit::Pool,
    fs::{
        op::{CompletionReceiver, IoBuf, IoKind, IoOp, IoStatus},
        scheduler::{alloc::SubmitterAlloc, BlockRef},
    },
    sched::TierPriority,
    sync::Arc,
};
use core::task::{Context, Poll};
use std::{collections::VecDeque, io};

/// Maximum number of blocks resident in the reorder window (submitted-but-not-yet-delivered). Bounds
/// the memory a slow consumer can pin: credit frees at completion, so without this a fast device
/// could accumulate unbounded completed-but-undelivered read buffers. Any value ≥ 1 keeps the stream
/// live (the front block always eventually completes and frees a slot); this is sized generously
/// because the per-device credit pool governs the healthy case and this only bites under a stall.
const MAX_RESIDENT_BLOCKS: usize = 256;

/// One unit of a materialize stream: either a block to **read** from a device, or **resident** bytes
/// the caller already holds in memory.
///
/// A storage service materializes objects whose blocks are a mix of on-disk extents and in-memory (cached or
/// just-written) data, delivered as one ordered stream. A [`Resident`](Block::Resident) block carries
/// its bytes directly and is delivered in FIFO order **without** touching `prepare`, the credit pool,
/// a device, or any backend — it bypasses the filesystem entirely, slotting into the reorder window
/// alongside the device reads so the consumer sees one seamless ordered stream.
#[derive(Clone, Debug)]
pub enum Block {
    /// Read this block from its device (credit-gated; runs through the backend).
    Read(BlockRef),
    /// Deliver these already-in-memory bytes in stream order, bypassing the filesystem. The caller is
    /// responsible for any trimming — they are delivered verbatim.
    Resident(ByteVec),
}

impl From<BlockRef> for Block {
    #[inline]
    fn from(block: BlockRef) -> Self {
        Block::Read(block)
    }
}

impl From<ByteVec> for Block {
    #[inline]
    fn from(bytes: ByteVec) -> Self {
        Block::Resident(bytes)
    }
}

/// Stream an ordered object whose blocks scatter across devices, delivering blocks in FIFO order via
/// **buffered** reads. Each block carries its own `Arc<Device>`, so the stream submits against each
/// block's device directly — no registry handle needed.
///
/// The source yields anything that converts into a [`Block`]: a [`BlockRef`] (read it from a device)
/// or a [`Block::Resident`] (already-in-memory bytes spliced into the stream in order, bypassing the
/// filesystem). A storage service interleaves both.
pub fn materialize<I>(blocks: I, priority: TierPriority) -> MaterializeStream<I::IntoIter>
where
    I: IntoIterator,
    I::Item: Into<Block>,
{
    MaterializeStream::new(blocks.into_iter(), priority, false)
}

/// Like [`materialize`] but issues **zero-copy `O_DIRECT`** reads into page-aligned buffers — the mode
/// a storage service uses. Each *read* block's `offset` and `len` must be block-aligned (a misaligned block
/// surfaces as an `InvalidInput` error in delivery order); resident blocks are unaffected by alignment.
pub fn materialize_direct<I>(blocks: I, priority: TierPriority) -> MaterializeStream<I::IntoIter>
where
    I: IntoIterator,
    I::Item: Into<Block>,
{
    MaterializeStream::new(blocks.into_iter(), priority, true)
}

/// A block pulled from the source but not yet enqueued: it is waiting for its device's acquire slot.
/// Carries the resolved pool + cost (validated at pull time) so the reactor enqueues without
/// re-validating.
struct PendingBlock {
    block: BlockRef,
    pool: Arc<Pool>,
    cost: u64,
}

/// A slot in the reorder window. We control the indices (dense, sequential) and deliver FIFO, so the
/// window is a `VecDeque` offset by the deliver cursor — slot `i` lives at `window[i - base]`.
enum Slot {
    /// Pulled from the source, awaiting its device's acquire slot before it can be enqueued.
    ToSubmit(PendingBlock),
    /// Enqueued (credit acquired, op in flight). Holds the block's trim metadata for completion.
    InFlight(BlockRef),
    /// Completed; the (trimmed) bytes or an error, awaiting its turn at the front.
    Ready(io::Result<ByteVec>),
}

/// Per-device submission state: the device's one reusable credit-acquire slot and the FIFO of window
/// indices awaiting submission on it. The acquire slot serves the queue front-to-back; a parked
/// acquire holds up only this device's queue, never another's.
struct DeviceSlot {
    alloc: SubmitterAlloc,
    to_submit: VecDeque<u64>,
}

impl DeviceSlot {
    fn new() -> Self {
        Self {
            alloc: SubmitterAlloc::new(),
            to_submit: VecDeque::new(),
        }
    }
}

/// An ordered stream over a scattered block list.
///
/// Poll it with [`MaterializeStream::next`]; each call resolves to the next block's bytes in FIFO
/// order (the `n`-th call yields the `n`-th block of `blocks`), or `None` at end of stream.
pub struct MaterializeStream<I> {
    blocks: I,
    priority: TierPriority,
    /// Whether to issue zero-copy `O_DIRECT` reads (page-aligned [`AlignedBuf`]) instead of buffered
    /// reads. A storage service uses `O_DIRECT`, so this is a first-class mode.
    ///
    /// [`AlignedBuf`]: crate::fs::direct::AlignedBuf
    direct: bool,
    /// Per-device submission state, indexed directly by the device's crate-internal
    /// [`index`](crate::fs::device::Device::index) (dense, monotonic from registration), lazily
    /// populated: each distinct device a block names gets its own reusable credit-acquire slot plus
    /// the FIFO of window indices awaiting submission on it. Keeping the acquire slot and its pending
    /// queue together is the unit a device is driven by — a device parked on credit does not
    /// head-of-line-block another device whose slot can grant. Direct array indexing (`Vec<Option>`
    /// grown on demand) — no map, no hash, no pointer scan.
    devices: Vec<Option<DeviceSlot>>,
    /// Single completion channel all sprayed blocks complete on; drained in `poll_next`.
    completion_rx: CompletionReceiver,
    /// Reorder window, front = the next block to deliver (block index `base`).
    window: VecDeque<Slot>,
    /// Block index of `window.front()` — the next block to hand to the consumer.
    base: u64,
    /// Set once the block iterator is exhausted.
    source_done: bool,
    /// Shared [flight-recorder](crate::fs::trace) grouping id stamped on every op this stream enqueues,
    /// so a dump can group one whole materialize run (`WHERE stream_id = N`). Minted once at
    /// construction; trace builds only.
    #[cfg(any(test, feature = "testing", feature = "io-dbg"))]
    stream_id: u64,
}

impl<I> MaterializeStream<I>
where
    I: Iterator,
    I::Item: Into<Block>,
{
    /// Build a materialize stream over `blocks`. Each block carries its own `Arc<Device>`, so the
    /// stream needs no registry handle — it submits against `block.device` directly. `direct` selects
    /// zero-copy `O_DIRECT` reads (the storage-service mode) vs. buffered reads.
    pub fn new(blocks: I, priority: TierPriority, direct: bool) -> Self {
        Self {
            blocks,
            priority,
            direct,
            devices: Vec::new(),
            completion_rx: crate::socket::channel::intrusive::datagram_completion::new::<IoOp>(),
            window: VecDeque::new(),
            base: 0,
            source_done: false,
            #[cfg(any(test, feature = "testing", feature = "io-dbg"))]
            stream_id: crate::fs::trace::next_stream_id(),
        }
    }

    /// The per-device slot index for `device` — its crate-internal registration index — growing the
    /// (lazily populated) `devices` vec and creating the slot on first use. O(1) direct indexing, no
    /// map or scan.
    fn device_slot(&mut self, device: &Arc<crate::fs::device::Device>) -> usize {
        let idx = device.index;
        if idx >= self.devices.len() {
            self.devices.resize_with(idx + 1, || None);
        }
        if self.devices[idx].is_none() {
            self.devices[idx] = Some(DeviceSlot::new());
        }
        idx
    }

    /// This stream's [flight-recorder](crate::fs::trace) grouping id (or
    /// [`NO_STREAM`](crate::fs::trace::NO_STREAM) outside the trace builds), passed to `enqueue` so
    /// every op of this run shares it.
    #[inline]
    fn trace_stream_id(&self) -> u64 {
        #[cfg(any(test, feature = "testing", feature = "io-dbg"))]
        {
            self.stream_id
        }
        #[cfg(not(any(test, feature = "testing", feature = "io-dbg")))]
        {
            crate::fs::trace::NO_STREAM
        }
    }

    /// Test-only: this stream's [flight-recorder](crate::fs::trace) grouping id, so a test can assert
    /// on *its own* run's ops rather than inferring the id from a process-global before/after diff
    /// (which is racy under concurrent tests sharing the recorder).
    #[cfg(test)]
    pub(crate) fn stream_id(&self) -> u64 {
        self.stream_id
    }

    /// Await the next block in FIFO order, or `None` at end of stream.
    pub async fn next(&mut self) -> Option<io::Result<ByteVec>> {
        core::future::poll_fn(|cx| self.poll_next(cx)).await
    }

    /// Single poll state machine: deliver the front block if ready, top up the read-ahead window,
    /// drive every device's acquire slot (out of order across devices), and drain completions. Loops
    /// while it makes progress; returns `Pending` once every wake source (each parked device slot and
    /// the completion channel) has the current waker registered.
    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<io::Result<ByteVec>>> {
        loop {
            // 1. Deliver the front block if it is already complete, advancing the cursor.
            if matches!(self.window.front(), Some(Slot::Ready(_))) {
                let Some(Slot::Ready(result)) = self.window.pop_front() else {
                    unreachable!("front matched Ready");
                };
                self.base += 1;
                return Poll::Ready(Some(result));
            }

            // 2. Top up the read-ahead window from the source (bounded by MAX_RESIDENT_BLOCKS).
            self.pull();
            // `pull` files a resident block, or a block that failed validation, `Ready` itself, and
            // no completion wakes the stream for it: deliver it now, since a `Pending` here would
            // leave a stream whose window was empty with no wake source registered for its front.
            if matches!(self.window.front(), Some(Slot::Ready(_))) {
                continue;
            }

            // 3. End of stream: source drained and the window fully delivered.
            if self.source_done && self.window.is_empty() {
                return Poll::Ready(None);
            }

            let mut progress = false;

            // 4. Drive each device's acquire slot. Out-of-order across devices: a device parked on
            //    credit does not block another device whose slot can grant.
            for dev in 0..self.devices.len() {
                if self.devices[dev].is_some() {
                    progress |= self.drive_device(cx, dev);
                }
            }

            // 5. Drain the entire completion queue in one atomic swap; file every landed op. Always
            //    re-registers the waker, so a later completion cannot be missed.
            match self.completion_rx.poll_swap(cx) {
                Poll::Ready(Some(mut queue)) => {
                    while let Some(entry) = queue.pop_front() {
                        self.land(entry.into_inner());
                    }
                    progress = true;
                }
                // The channel only closes if every sender (and the scheduler) is gone while we still
                // expect completions — surface as end-of-stream.
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => {}
            }

            // 6. Loop if anything advanced; otherwise every wake source is armed — park.
            if !progress {
                return Poll::Pending;
            }
        }
    }

    /// Pull blocks from the source into the window + per-device submit queues until the read-ahead cap
    /// is hit or the source is exhausted. A block that fails validation is filed `Ready(Err)` in place
    /// so the consumer observes it in delivery order rather than as a silent gap.
    fn pull(&mut self) {
        while !self.source_done && self.window.len() < MAX_RESIDENT_BLOCKS {
            let Some(item) = self.blocks.next() else {
                self.source_done = true;
                break;
            };
            let item: Block = item.into();
            let idx = self.base + self.window.len() as u64;

            // Resident bytes bypass the filesystem entirely: file them `Ready` straight into the
            // reorder window so they deliver in FIFO order alongside the device reads — no `prepare`,
            // no credit, no device, no backend. This is the in-memory / cached / just-written path
            // A storage service interleaves with on-disk extents.
            let block = match item {
                Block::Read(block) => block,
                Block::Resident(bytes) => {
                    self.window.push_back(Slot::Ready(Ok(bytes)));
                    continue;
                }
            };

            // A direct read requires `offset` and `len` to be block-aligned; `prepare` rejects a
            // misaligned op (it does not round anything up — the caller sizes the block). Buffered
            // reads have no alignment constraint.
            match block
                .device
                .prepare(IoKind::Read, block.offset, block.len, self.direct)
            {
                Ok((pool, cost)) => {
                    // Find (or create) this device's acquire slot by `Arc` identity and queue the
                    // block on it.
                    let dev = self.device_slot(&block.device);
                    self.devices[dev].as_mut().unwrap().to_submit.push_back(idx);
                    self.window
                        .push_back(Slot::ToSubmit(PendingBlock { block, pool, cost }));
                }
                Err(e) => self.window.push_back(Slot::Ready(Err(e))),
            }
        }
    }

    /// Drive one device's acquire slot: submit as many of its queued blocks as its credit allows,
    /// stopping when the slot parks (Pending) or the device's queue empties. Returns whether it
    /// enqueued (or failed) at least one block this call.
    fn drive_device(&mut self, cx: &mut Context<'_>, dev: usize) -> bool {
        // The slot is always `Some` here: `pull` creates it before queueing any block for `dev`, and
        // `poll_next` only drives indices that are populated.
        debug_assert!(
            self.devices.get(dev).is_some_and(Option::is_some),
            "drive_device on an unpopulated slot {dev}"
        );
        let mut progress = false;
        loop {
            let Some(&idx) = self.devices[dev].as_ref().and_then(|s| s.to_submit.front()) else {
                return progress;
            };
            let pos = (idx - self.base) as usize;
            // Clone the block (it carries an `Arc<Device>`) plus the resolved pool + cost out from
            // under the borrow without moving the slot — the slot is overwritten only on a successful
            // acquire.
            let (block, pool, cost) = match &self.window[pos] {
                Slot::ToSubmit(p) => (p.block.clone(), p.pool.clone(), p.cost),
                _ => {
                    debug_assert!(false, "materialize: device queue index {idx} not ToSubmit");
                    self.devices[dev].as_mut().unwrap().to_submit.pop_front();
                    continue;
                }
            };

            let acquired = {
                let alloc = &mut self.devices[dev].as_mut().unwrap().alloc;
                alloc.set_trace_ctx(block.device.index, IoKind::Read);
                alloc.poll_acquire(cx, &pool, cost, self.priority)
            };
            match acquired {
                Poll::Ready(Ok(())) => {
                    let slot = self.devices[dev].as_mut().unwrap();
                    let granted = slot.alloc.take_all();
                    debug_assert!(granted >= cost, "acquire returned less than cost");
                    slot.to_submit.pop_front();
                    let buf = if self.direct {
                        IoBuf::Direct(crate::fs::direct::AlignedBuf::new(block.len as usize))
                    } else {
                        // Pre-sized, logically-empty buffer: the backend reads into spare capacity
                        // and sets the length to bytes read (no zero-fill, short read leaves no tail).
                        IoBuf::Read(bytes::BytesMut::with_capacity(block.len as usize))
                    };
                    let r = block.device.enqueue(
                        IoKind::Read,
                        block.fd.clone(),
                        block.offset,
                        block.len,
                        buf,
                        self.completion_rx.sender(),
                        idx,
                        granted,
                        self.trace_stream_id(),
                    );
                    self.window[pos] = Slot::InFlight(block);
                    if let Err(e) = r {
                        // Submission channel closed: file the error in place (it stays in delivery
                        // order). The credit was released by `enqueue`.
                        self.window[pos] = Slot::Ready(Err(e));
                    }
                    progress = true;
                }
                Poll::Ready(Err(e)) => {
                    // Pool closed: file the error in place and drop the block from the queue.
                    self.devices[dev].as_mut().unwrap().to_submit.pop_front();
                    self.window[pos] = Slot::Ready(Err(e));
                    progress = true;
                }
                // Slot parked on credit (waker registered). Stop driving this device; another
                // device's slot may still make progress.
                Poll::Pending => return progress,
            }
        }
    }

    /// File a completed op into its window slot, computed from its block index (`user_data`). The
    /// index invariants are `debug_assert`s rather than silent `return`s: a completion for an
    /// already-delivered or out-of-range block, a duplicate, or a still-pending op is impossible by
    /// construction (a slot is delivered only once `Ready`; indices are dense and monotonic). If one
    /// occurs it is a routing/index bug, and we want it to fail loudly in tests rather than corrupt
    /// the delivered stream.
    fn land(&mut self, op: IoOp) {
        let idx = op.user_data;
        debug_assert!(
            idx >= self.base,
            "materialize: completion for already-delivered block {idx} (base {})",
            self.base
        );
        let Some(pos) = idx.checked_sub(self.base).map(|p| p as usize) else {
            return;
        };
        debug_assert!(
            pos < self.window.len(),
            "materialize: completion index {idx} past window end (base {}, len {})",
            self.base,
            self.window.len()
        );
        if pos >= self.window.len() {
            return;
        }
        // Only the trim metadata is needed to finish the buffer; read it out without cloning the
        // block's `Arc<Device>`.
        let (head_trim, tail_trim) = match &self.window[pos] {
            Slot::InFlight(b) => (b.head_trim, b.tail_trim),
            _ => {
                debug_assert!(
                    false,
                    "materialize: completion for non-in-flight block {idx}"
                );
                return;
            }
        };
        let result = match op.status {
            IoStatus::Done(_) => Ok(finish(op.buf, head_trim, tail_trim)),
            IoStatus::Failed(kind) => Err(kind.into()),
            IoStatus::Pending => {
                debug_assert!(
                    false,
                    "materialize: landed a still-pending op for block {idx}"
                );
                return;
            }
        };
        self.window[pos] = Slot::Ready(result);
    }
}

/// Turn a completed read buffer into the delivered [`ByteVec`], applying head/tail trim. Buffered
/// reads freeze their `BytesMut` (zero-copy); direct reads hand the page-aligned `AlignedBuf` to
/// `Bytes::from_owner` (zero-copy — the buffer is moved, not copied). Either way the single chunk
/// becomes a `ByteVec`, trimmed via `advance`/`truncate` (no copy — both adjust chunk bounds).
fn finish(buf: IoBuf, head_trim: u32, tail_trim: u32) -> ByteVec {
    let bytes: bytes::Bytes = match buf {
        IoBuf::Read(b) => b.freeze(),
        // `from_owner` exposes the buffer's logical length (set to the bytes actually read by the
        // backend) without a copy.
        IoBuf::Direct(b) => bytes::Bytes::from_owner(b),
        _ => bytes::Bytes::new(),
    };
    trim(bytes.into(), head_trim as usize, tail_trim as usize)
}

/// Apply head/tail trim to a block's bytes (for sub-range reads whose edge blocks are partial).
///
/// `head`/`tail` were computed against the block's *requested* length. If the backend returned a
/// **short** read (fewer bytes than requested — only possible at EOF, since a mid-file block is
/// always fully present), `len` here is the short count: a `head` past the returned data yields an
/// empty buffer and the tail trim is clamped. Panic-safe (every bound is clamped), and for a storage service's
/// interior reads no trim/short-read interaction arises. `advance`/`truncate` are zero-copy (they
/// adjust chunk boundaries, never memcpy).
fn trim(mut bytes: ByteVec, head: usize, tail: usize) -> ByteVec {
    let len = bytes.len();
    if head >= len {
        return ByteVec::new();
    }
    // Drop the tail first (so head offsets are still measured against the original front), then the
    // head. `tail` is clamped so we never truncate below `head`.
    let keep = len.saturating_sub(tail).max(head);
    bytes.truncate(keep);
    // `head < len` and `head <= keep`, so advancing `head` bytes cannot exceed the buffer.
    let _ = bytes.advance(head);
    bytes
}
