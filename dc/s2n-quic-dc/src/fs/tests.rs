// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Bach smoke tests for the IO scheduler.
//!
//! These run under the deterministic discrete-event simulator and assert the M1 properties: the
//! credit pool's fair share across equal-priority streams, strict priority across tiers, per-device
//! isolation, cooperative backpressure (no thread blowup), ordered spray for `materialize`, and
//! credit conservation (acquire-on-submit / release-on-complete, zero leak at quiescence).

use crate::{
    counter::Registry,
    fs::{
        backend::{
            bach::{BachBackend, Latency},
            Backend, LaneSetup,
        },
        config::{CostModel, DeviceConfig, OpWeights, PoolMode},
        device::Device,
        materialize::materialize,
        op::{IoOp, IoStatus},
        scheduler::{BlockRef, DeviceRegistry},
    },
    sched::{CreditConfig, Rate, TierPriority},
    testing::{ext::*, sim},
    time::bach::Clock,
    tracing::*,
};
use core::time::Duration;
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// A backend whose lanes are immediately dead: it builds the lane submit senders but drops the
/// matching receivers right away (modelling a lane task that failed/exited while the scheduler keeps
/// accepting submits — e.g. a syscall/uring lane that hit a fatal error in M2/M3). The dispatch task's
/// `lane.send(...)` then returns `Err(op)` and the op is routed to the completion sink stamped Failed.
struct DeadLaneBackend;

impl Backend for DeadLaneBackend {
    type Lane =
        crate::socket::channel::intrusive::unsync::Sender<crate::intrusive::EntryAdapter<IoOp>>;

    fn spawn_lanes(&self, setup: LaneSetup) -> Vec<Self::Lane> {
        let mut handles = Vec::with_capacity(setup.lane_count);
        for _ in 0..setup.lane_count {
            let (tx, rx) = crate::socket::channel::intrusive::unsync::new::<IoOp>();
            // Drop the receiver immediately: the lane channel is now closed, so every send fails.
            drop(rx);
            handles.push(tx);
        }
        handles
    }
}

/// A bach backend whose processor stamps every op `Failed` (modelling a real backend whose syscall
/// errored — e.g. EIO / O_DIRECT EINVAL). The old dedicated `FailBackend` is now just `BachBackend`
/// with a failing processor closure.
fn fail_backend(
    clock: Clock,
    kind: std::io::ErrorKind,
) -> BachBackend<Clock, impl Fn(&mut IoOp) + Clone> {
    BachBackend::with_processor(clock, Latency::default(), move |op: &mut IoOp| {
        op.status = IoStatus::Failed(kind);
    })
}

/// Build a scheduler with a `Bach` backend at the given latency and `ring_count`, then register each
/// device config (labelled `dev{i}`) and return the scheduler alongside the registered device
/// handles. Tests submit against `&devices[i]` and conservation-check the held handles directly.
fn build(
    device_cfgs: Vec<DeviceConfig>,
    lane_count: usize,
    latency: Latency,
) -> (DeviceRegistry, Vec<Arc<Device>>) {
    let clock = Clock::default();
    let backend = BachBackend::new(clock.clone(), latency);
    let mut spawn = bach_spawner();
    let registry = DeviceRegistry::new(backend, &mut spawn, &Registry::default(), clock);
    let devices = device_cfgs
        .iter()
        .enumerate()
        .map(|(i, cfg)| {
            registry
                .register_device(
                    format!("dev{i}").as_str(),
                    &cfg.clone().with_lane_count(lane_count),
                )
                .unwrap()
        })
        .collect();
    (registry, devices)
}

/// Assert every pool of `device` returned to full capacity (no credit leak).
fn assert_conserved(device: &Arc<Device>) {
    for pool in device.pools.all() {
        assert_eq!(
            pool.debug_free_total(),
            pool.debug_capacity() as i64,
            "credit leaked on device {:?}",
            device.label
        );
    }
}

/// The bach spawner used by every sim test (drives `!Send` futures on the bach executor).
fn bach_spawner() -> crate::runtime::bach::Local {
    crate::runtime::bach::Local::new(0)
}

/// A submit whose op cannot be handed to its lane (closed lane channel — e.g. a syscall/uring lane
/// that hit a fatal error in M2/M3) must (1) fail the submit promptly with `Err` rather than hang
/// the parked future forever, and (2) release the op's borrowed credit so the pool does not leak.
#[test]
fn lane_send_failure_fails_submit_and_conserves_credit() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let clock = Clock::default();
        let mut spawn = bach_spawner();
        let scheduler =
            DeviceRegistry::new(DeadLaneBackend, &mut spawn, &Registry::default(), clock);
        let dev = scheduler.register_device("busy", &iops_device(8)).unwrap();

        async move {
            // Each read acquires credit, fails to push to the dead lane, and must resolve to Err
            // promptly (not hang). `.await` directly — if the future hung, the sim would never
            // reach quiescence and the test would time out.
            for i in 0..5u64 {
                let result = dev.read(fd(), i * 4096, 4096, TierPriority::Medium).await;
                assert!(
                    result.is_err(),
                    "submit to a closed lane must fail, not succeed or hang"
                );
            }
            // Conservation: every acquired credit was released back to the pool on the failed path.
            info!("lane-send-failure conservation check");
            assert_conserved(&dev);
        }
        .primary()
        .spawn();
    });
}

/// REGRESSION (swarm review, correctness): an op whose cost exceeds the credit pool's
/// **`max_single_acquire`** (but not its capacity) must still be granted atomically and complete —
/// it must NOT fragment into partial grants that pin the pool and wedge.
///
/// The public `DeviceConfig::shared_bytes` constructor uses `CreditConfig::new`, whose default
/// per-priority caps are `capacity/256` (Medium) .. `capacity/64`. The acquire path clamps each
/// request to `max_single_acquire`, so without the fix an op with `max_single_acquire < cost <=
/// capacity` accumulates `max_single_acquire`-sized partial grants in its alloc, never reaching
/// `cost`; with enough concurrent such ops every byte is pinned across parked allocs, nothing is in
/// flight to release, and the pool deadlocks (the exact partial-pin wedge `atomic_grant` exists to
/// prevent — but which it only closed on the grant side, not the acquire/clamp side).
///
/// Here capacity = 256 KiB (so Medium `max_single_acquire` = 1 KiB) and three concurrent 128 KiB
/// reads (≫ the 1 KiB cap, and 1.5× capacity in aggregate so they must serialize through the pool).
/// Before the fix this completes 0 of 3 (hang → sim never quiesces → timeout); after, all 3 complete.
#[test]
fn op_larger_than_max_single_acquire_grants_atomically() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        // Public constructor → default (non-uniform, capacity-scaled) caps. Refill off so only the
        // grant machinery (not the liveness pacer) can make progress — isolating the wedge.
        let mut cfg = DeviceConfig::shared_bytes(256 * 1024, Rate::new(1_000_000.0));
        let PoolMode::Shared(c) = cfg.pool_mode else {
            unreachable!()
        };
        cfg.pool_mode = PoolMode::Shared(c.without_refill());

        let (_scheduler, devices) = {
            let clock = Clock::default();
            // Latency long enough that all three ops are concurrently in flight / contending.
            let latency = Latency {
                read: Duration::from_micros(200),
                write: Duration::from_micros(200),
                per_byte_nanos: 0,
            };
            let backend = BachBackend::new(clock.clone(), latency);
            let mut spawn = bach_spawner();
            let s = DeviceRegistry::new(backend, &mut spawn, &Registry::default(), clock);
            let dev = s.register_device("bulk", &cfg.with_lane_count(2)).unwrap();
            (s, vec![dev])
        };
        let dev = devices[0].clone();

        let completed = Rc::new(std::cell::Cell::new(0u64));
        for i in 0..3u64 {
            let dev = dev.clone();
            let completed = completed.clone();
            async move {
                // 128 KiB ≫ Medium max_single_acquire (1 KiB) but < capacity (256 KiB).
                if dev
                    .read(fd(), i * 128 * 1024, 128 * 1024, TierPriority::Medium)
                    .await
                    .is_ok()
                {
                    completed.set(completed.get() + 1);
                }
            }
            .primary()
            .spawn();
        }

        let completed_r = completed.clone();
        async move {
            50.ms().sleep().await;
            assert_eq!(
                completed_r.get(),
                3,
                "ops larger than max_single_acquire wedged the pool (partial-pin deadlock)"
            );
            assert_conserved(&dev);
            info!("large-op atomic grant: all completed, pool conserved");
        }
        .primary()
        .spawn();
    });
}

/// A tiny IOPS device: `queue_depth` outstanding ops, one IOP per 4 KiB, paced generously so the
/// credit pool (not the pacer) governs concurrency in these tests.
fn iops_device(queue_depth: u64) -> DeviceConfig {
    DeviceConfig {
        // Floor the fair-share slice at 1 op and cap a single acquire at 1 op so each block read is
        // its own credit unit — the cleanest shape for observing fair share across streams.
        pool_mode: PoolMode::Shared(
            CreditConfig::new(queue_depth)
                .with_max_single_acquire_uniform(queue_depth.max(1))
                .with_min_grant_slice_uniform(1)
                .without_refill(),
        ),
        rate: Rate::new(100.0),
        cost_model: CostModel::Iops { io_unit: 4096 },
        op_weights: OpWeights::default(),
        lane_count: 1,
    }
}

/// A throwaway [`Fd`](crate::fs::op::Fd) for the bach tests: the bach backend fills buffers
/// synthetically and never issues a real syscall, so the descriptor is never dereferenced.
fn fd() -> crate::fs::op::Fd {
    struct DummyFd;
    impl std::os::fd::AsRawFd for DummyFd {
        fn as_raw_fd(&self) -> std::os::fd::RawFd {
            7
        }
    }
    crate::fs::op::Fd::new(Arc::new(DummyFd))
}

/// Poll `future` once with a no-op waker and assert it parks (`Pending`). Used to drive a one-shot
/// submit future (`Device::read`/`write`/`*_direct`) to a specific await point so the test can drop
/// it *there* and assert the cancellation is clean. (A no-op waker is correct precisely because we
/// never intend to re-poll — we drop the future at the park point.)
fn poll_parks<F: core::future::Future>(future: core::pin::Pin<&mut F>) {
    let waker = s2n_quic_core::task::waker::noop();
    let mut cx = core::task::Context::from_waker(&waker);
    assert!(
        future.poll(&mut cx).is_pending(),
        "future was expected to park at this await point"
    );
}

/// CANCEL SAFETY — drop while parked on credit (op never enqueued).
///
/// A `Device::write`/`read` future dropped while it is parked acquiring credit — *before* its `IoOp`
/// was ever built/enqueued — must: release no credit it does not hold, strand no waiter slot, and
/// leave the device fully functional. At this await point the buffer still lives in the future's own
/// frame (no op exists yet), so the drop simply frees it; the parked credit slot is abandoned and
/// reclaimed by the pool's next grant walk. This is the "decide not to go through with the write
/// while waiting for credits" case.
#[test]
fn drop_future_while_parked_on_credit_conserves_and_recovers() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        // depth-1 device + long latency: one in-flight op saturates the pool and stays out for 50ms,
        // so a second future has no credit and parks on acquire.
        let latency = Latency {
            read: Duration::from_millis(50),
            write: Duration::from_millis(50),
            per_byte_nanos: 0,
        };
        let (scheduler, devices) = build(vec![iops_device(1)], 1, latency);
        let dev = devices[0].clone();

        // Occupant: holds the single credit in flight for 50ms. (Reads cost 1 credit; a write would
        // cost 2 — the default 2× write weight — and exceed this depth-1 pool.)
        {
            let dev = dev.clone();
            async move {
                let _ = dev.read(fd(), 0, 4096, TierPriority::Medium).await;
            }
            .primary()
            .spawn();
        }

        async move {
            // Keep the scheduler (and its pool) alive for the whole sim; dropped when this task ends.
            let _scheduler = scheduler;
            // Let the occupant acquire the only credit and park on its 50ms completion.
            1.ms().sleep().await;
            assert!(
                dev.pools
                    .all()
                    .any(|p| p.debug_free_total() < p.debug_capacity() as i64),
                "precondition: occupant should hold the only credit"
            );

            // A second read now parks on credit (pool empty). Poll it to the park point, then drop it
            // there — the op was never enqueued, so this drops the buffer and abandons the slot.
            {
                let mut fut = core::pin::pin!(dev.read(fd(), 4096, 4096, TierPriority::Medium));
                poll_parks(fut.as_mut());
                // `fut` (and its buffer) drops at the end of this block, abandoning the slot.
            }

            // Occupant completes and releases its credit; the abandoned waiter slot is reclaimed by
            // the distributor's grant walk rather than being handed (and losing) the credit.
            100.ms().sleep().await;
            assert_conserved(&dev);

            // The device still works after the mid-acquire cancellation.
            dev.read(fd(), 8192, 4096, TierPriority::Medium)
                .await
                .unwrap();
            assert_conserved(&dev);
            info!("drop-while-parked-on-credit: conserved and recovered");
        }
        .primary()
        .spawn();
    });
}

/// CANCEL SAFETY — drop after enqueue, with the op in flight (the io_uring-queue case).
///
/// A `Device::write` future dropped *after* its op has been handed to the pipeline (credit acquired,
/// op + buffer enqueued) and while it awaits completion must conserve credit and leave the device
/// functional. The crucial property: the buffer is owned by the in-flight `IoOp` **downstream** — not
/// by the future — so dropping the future cannot pull the buffer out from under a backend (or, on the
/// uring backend, an in-kernel) operation. The op finishes downstream, `complete`/`complete_cancelled`
/// releases its credit, and the now-orphaned completion is discarded on the dead receiver.
#[test]
fn drop_future_with_op_in_flight_conserves_and_recovers() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let latency = Latency {
            read: Duration::from_millis(50),
            write: Duration::from_millis(50),
            per_byte_nanos: 0,
        };
        let (scheduler, devices) = build(vec![iops_device(8)], 1, latency);
        let dev = devices[0].clone();

        async move {
            let _scheduler = scheduler;
            {
                // A single poll drives the future through credit acquire + enqueue (the op and its
                // buffer are now downstream) and parks it on `await_completion`.
                let mut fut = core::pin::pin!(dev.write(
                    fd(),
                    0,
                    bytes::Bytes::from(vec![7u8; 4096]),
                    TierPriority::Medium
                ));
                poll_parks(fut.as_mut());
                assert!(
                    dev.pools
                        .all()
                        .any(|p| p.debug_free_total() < p.debug_capacity() as i64),
                    "precondition: op should hold credit (enqueued, in flight) before the drop"
                );
                // Drop with the op in flight (at end of block): the buffer rides the op, not the
                // future.
            }

            // The in-flight op completes downstream; its credit is released before the completion is
            // discarded on the dead receiver (or the backend skips it as cancelled — either conserves).
            100.ms().sleep().await;
            assert_conserved(&dev);

            // The device still works after the in-flight cancellation.
            let buf = dev.read(fd(), 0, 4096, TierPriority::Medium).await.unwrap();
            assert_eq!(buf.len(), 4096);
            assert_conserved(&dev);
            info!("drop-with-op-in-flight: conserved and recovered");
        }
        .primary()
        .spawn();
    });
}

/// RESERVE-THEN-DROP — a reservation dropped without `submit` is a cancellation: the held credit is
/// released back to the pool and **no IO is issued**. This is the spill-flush cancel case in the
/// two-phase model — the data died after credit was acquired but before the buffer was committed, so we
/// drop the reservation and the device sees zero wasted writes.
///
/// We reserve credit on a device, assert the pool is drawn down (the grant is held), then drop the
/// reservation and confirm the backend processor never ran and the pool returned to full.
#[test]
fn reservation_drop_cancels_without_io() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let ran = Rc::new(std::cell::Cell::new(0u64));
        let ran_proc = ran.clone();
        let clock = Clock::default();
        let backend =
            BachBackend::with_processor(clock.clone(), Latency::default(), move |op: &mut IoOp| {
                ran_proc.set(ran_proc.get() + 1);
                crate::fs::backend::bach::process_default(op);
            });
        let mut spawn = bach_spawner();
        let scheduler = DeviceRegistry::new(backend, &mut spawn, &Registry::default(), clock);
        let dev = scheduler.register_device("dev0", &iops_device(8)).unwrap();

        async move {
            let _scheduler = scheduler;
            let reservation = dev
                .reserve(
                    crate::fs::op::IoKind::Read,
                    fd(),
                    0,
                    4096,
                    TierPriority::Medium,
                    false,
                )
                .await
                .expect("reserve should acquire credit");
            // The grant is held by the reservation: the pool is drawn down.
            assert!(
                dev.pools
                    .all()
                    .any(|p| p.debug_free_total() < p.debug_capacity() as i64),
                "reservation should hold credit (pool drawn down)"
            );

            // Drop = cancel: no buffer was ever committed, so no op is built and no IO is issued.
            drop(reservation);

            10.ms().sleep().await;
            assert_eq!(ran.get(), 0, "a dropped reservation must not run any IO");
            // Credit released back on drop → pool conserved.
            assert_conserved(&dev);
            info!("reservation drop: cancelled without IO, credit conserved");
        }
        .primary()
        .spawn();
    });
}

/// RESERVE-WHILE-PARKED THEN DROP — a reservation can be cancelled even while its `reserve` future is
/// still parked on credit: dropping the future abandons the credit slot cleanly. This is the spill
/// backlog case where the data dies *before* the credit pool ever has room — we never want to issue the
/// write, and we want the parked slot reclaimed for live writes.
///
/// A depth-1 device with a 50ms occupant forces the second reserve to park on credit. We race that
/// `reserve().await` against a 25ms timer and drop it when the timer wins — strictly before any grant —
/// then confirm the offset never executed and the pool conserved once the occupant releases. (Default
/// write weight is 2x, so depth-1 pool tests must use reads.)
#[test]
fn reservation_parked_drop_cancels_without_io() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let latency = Latency {
            read: Duration::from_millis(50),
            write: Duration::from_millis(50),
            per_byte_nanos: 0,
        };
        // Record the offsets the backend actually executes. The occupant (offset 0) runs; the parked,
        // dropped reserve (offset 4096) must NOT — so 4096 never appears.
        let ran_offsets: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
        let ran_proc = ran_offsets.clone();
        let clock = Clock::default();
        let backend = BachBackend::with_processor(clock.clone(), latency, move |op: &mut IoOp| {
            ran_proc.borrow_mut().push(op.offset);
            crate::fs::backend::bach::process_default(op);
        });
        let mut spawn = bach_spawner();
        let scheduler = DeviceRegistry::new(backend, &mut spawn, &Registry::default(), clock);
        let dev = scheduler.register_device("dev0", &iops_device(1)).unwrap();

        // Occupant: holds the single credit in flight for 50ms so the second reserve parks on credit.
        {
            let dev = dev.clone();
            async move {
                let _ = dev.read(fd(), 0, 4096, TierPriority::Medium).await;
            }
            .primary()
            .spawn();
        }

        async move {
            let _scheduler = scheduler;
            // Let the occupant deterministically acquire the only credit before we reserve, so our
            // reserve parks on credit rather than winning it.
            1.ms().sleep().await;
            assert!(
                dev.pools
                    .all()
                    .any(|p| p.debug_free_total() < p.debug_capacity() as i64),
                "precondition: occupant should hold the only credit so the next reserve parks"
            );

            // Race the parked reserve against a 25ms timer (occupant releases only at 50ms), so the
            // timer wins and we drop the still-parked reserve future — strictly before any grant.
            let reserve = dev.reserve(
                crate::fs::op::IoKind::Read,
                fd(),
                4096,
                4096,
                TierPriority::Medium,
                false,
            );
            tokio::select! {
                _ = reserve => panic!(
                    "reserve must not win against the 25ms timer (occupant holds credit until 50ms)"
                ),
                _ = 25.ms().sleep() => {}
            }

            // Let the occupant finish and any grant settle.
            50.ms().sleep().await;
            assert!(
                !ran_offsets.borrow().contains(&4096),
                "the dropped parked reserve (offset 4096) must issue no IO; ran offsets: {:?}",
                ran_offsets.borrow()
            );
            assert_conserved(&dev);
            info!("reservation parked drop: cancelled without IO, credit conserved");
        }
        .primary()
        .spawn();
    });
}

/// SUBMIT PATH — a reservation that is `submit`ted (rather than dropped) behaves exactly like a one-shot
/// `submit`: the op runs to completion and the buffer comes back. Guards against the two-phase split
/// regressing the normal path, for both a buffered op and a zero-copy `O_DIRECT` write.
#[test]
fn reservation_submit_completes() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let (scheduler, devices) = build(vec![iops_device(8)], 1, Latency::default());
        let dev = devices[0].clone();
        async move {
            let _scheduler = scheduler;
            // reserve + submit a buffered read completes like `submit`.
            let op = dev
                .reserve(
                    crate::fs::op::IoKind::Read,
                    fd(),
                    0,
                    4096,
                    TierPriority::Medium,
                    false,
                )
                .await
                .expect("reserve")
                .submit(crate::fs::op::IoBuf::Read(bytes::BytesMut::with_capacity(
                    4096,
                )))
                .await
                .expect("submit");
            assert!(matches!(op.status, IoStatus::Done(4096)));

            // reserve + submit a zero-copy O_DIRECT write writes and returns the buffer.
            let buf = crate::fs::direct::AlignedBuf::new(crate::fs::direct::ALIGNMENT);
            let len = buf.len() as u32;
            let op = dev
                .reserve(
                    crate::fs::op::IoKind::Write,
                    fd(),
                    0,
                    len,
                    TierPriority::Medium,
                    true,
                )
                .await
                .expect("reserve")
                .submit(crate::fs::op::IoBuf::Direct(buf))
                .await
                .expect("submit");
            match (op.status, op.buf) {
                (IoStatus::Done(n), crate::fs::op::IoBuf::Direct(_)) => {
                    assert_eq!(n, crate::fs::direct::ALIGNMENT)
                }
                other => panic!("unexpected direct write completion: {other:?}"),
            }
            assert_conserved(&dev);
            info!("reservation submit: completed normally");
        }
        .primary()
        .spawn();
    });
}

/// Run `n` identical reader streams at `priority` against `device` for a fixed simulated window,
/// returning each stream's completion count. Each stream keeps exactly one op in flight (submit →
/// await → submit), so the concurrent demand equals the stream count; set the device queue depth
/// below that to force credit contention.
fn run_streams(
    device: DeviceConfig,
    streams: Vec<(&'static str, TierPriority)>,
    window: Duration,
) -> Rc<RefCell<Vec<(&'static str, u64)>>> {
    let (scheduler, devices) = build(vec![device], 1, Latency::default());
    let dev = devices[0].clone();
    let counts: Rc<RefCell<Vec<(&'static str, u64)>>> = Rc::new(RefCell::new(Vec::new()));

    for (label, priority) in streams {
        let counts = counts.clone();
        let dev = dev.clone();
        async move {
            let mut done = 0u64;
            let start = bach::time::Instant::now();
            let mut offset = 0u64;
            while start.elapsed() < window {
                if dev.read(fd(), offset, 4096, priority).await.is_ok() {
                    done += 1;
                    offset += 4096;
                }
            }
            counts.borrow_mut().push((label, done));
        }
        .primary()
        .spawn();
    }

    // Keep the scheduler (and its device) alive past the window for the conservation check.
    let counts_ret = counts.clone();
    async move {
        (window.as_millis() as u64 + 10).ms().sleep().await;
        // Conservation: at quiescence every acquired credit was released back to the pool.
        assert_conserved(&dev);
        info!("device pool conserved at quiescence");
        drop(scheduler);
    }
    .primary()
    .spawn();

    counts_ret
}

/// Demand-elastic fair share *within a tier*: four equal-priority streams contend for a device
/// whose queue depth (2) is below their combined demand (4). The distributor's `slice =
/// free/num_waiters` round-robin should hand all four near-equal throughput.
#[test]
fn fair_share_within_tier() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let counts = run_streams(
            iops_device(2),
            vec![
                ("a", TierPriority::Medium),
                ("b", TierPriority::Medium),
                ("c", TierPriority::Medium),
                ("d", TierPriority::Medium),
            ],
            40.ms(),
        );
        async move {
            55.ms().sleep().await;
            let mut c = counts.borrow().clone();
            c.sort();
            let counts_only: Vec<u64> = c.iter().map(|(_, n)| *n).collect();
            let (min, max) = (
                *counts_only.iter().min().unwrap(),
                *counts_only.iter().max().unwrap(),
            );
            info!(?counts_only, min, max, "fair share within tier");
            assert!(min > 0, "a contending stream was starved within its tier");
            assert!(
                max - min <= (max / 4).max(2),
                "unfair within tier: spread {min}..{max}"
            );
        }
        .primary()
        .spawn();
    });
}

/// Strict priority *across tiers*: with the queue depth (2) below combined demand (4), the two
/// `High` streams should out-complete the two `Background` streams.
#[test]
fn strict_priority_across_tiers() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let counts = run_streams(
            iops_device(2),
            vec![
                ("high-a", TierPriority::High),
                ("high-b", TierPriority::High),
                ("bg-a", TierPriority::Background),
                ("bg-b", TierPriority::Background),
            ],
            40.ms(),
        );
        async move {
            55.ms().sleep().await;
            let c = counts.borrow().clone();
            let get = |name: &str| {
                c.iter()
                    .find(|(l, _)| *l == name)
                    .map(|(_, n)| *n)
                    .unwrap_or(0)
            };
            let high = get("high-a") + get("high-b");
            let bg = get("bg-a") + get("bg-b");
            info!(high, bg, "strict priority across tiers");
            assert!(
                high > bg,
                "High tier did not out-complete Background ({high} vs {bg})"
            );
        }
        .primary()
        .spawn();
    });
}

/// Device isolation: a saturated device must not steal a second device's budget.
#[test]
fn device_isolation() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let (scheduler, devices) =
            build(vec![iops_device(2), iops_device(2)], 2, Latency::default());
        let busy = devices[0].clone();
        let quiet = devices[1].clone();

        let quiet_done = Rc::new(RefCell::new(0u64));

        for _ in 0..4 {
            let busy = busy.clone();
            async move {
                let start = bach::time::Instant::now();
                let mut offset = 0u64;
                while start.elapsed() < 40.ms() {
                    let _ = busy.read(fd(), offset, 4096, TierPriority::Medium).await;
                    offset += 4096;
                }
            }
            .primary()
            .spawn();
        }

        {
            let quiet_done = quiet_done.clone();
            let quiet = quiet.clone();
            async move {
                let start = bach::time::Instant::now();
                let mut offset = 0u64;
                let mut n = 0u64;
                while start.elapsed() < 40.ms() {
                    if quiet
                        .read(fd(), offset, 4096, TierPriority::Medium)
                        .await
                        .is_ok()
                    {
                        n += 1;
                        offset += 4096;
                    }
                }
                *quiet_done.borrow_mut() = n;
            }
            .primary()
            .spawn();
        }

        let quiet_done_r = quiet_done.clone();
        async move {
            50.ms().sleep().await;
            let n = *quiet_done_r.borrow();
            info!(quiet_completed = n, "isolation: quiet device throughput");
            assert!(n > 0, "quiet device starved by busy device");
            assert_conserved(&busy);
            assert_conserved(&quiet);
            info!("isolation: both device pools conserved");
            drop(scheduler);
        }
        .primary()
        .spawn();
    });
}

/// Ordered spray: a `materialize` stream over blocks scattered across two devices, with the bach
/// backend completing blocks out of order, must deliver bytes in strict FIFO order. The deterministic
/// fill (`byte i = (offset+i) as u8`) lets us verify each block.
#[test]
fn materialize_delivers_in_order() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let latency = Latency {
            read: Duration::from_micros(100),
            write: Duration::from_micros(100),
            per_byte_nanos: 0,
        };
        let (_scheduler, devices) = build(vec![iops_device(8), iops_device(8)], 2, latency);

        async move {
            // 10 blocks of 1 KiB, alternating devices so they land on different lanes and complete
            // out of order; offsets are strictly increasing in submission order.
            let blocks: Vec<BlockRef> = (0..10u64)
                .map(|i| {
                    let dev = devices[(i % 2) as usize].clone();
                    BlockRef::whole(dev, fd(), i * 1024, 1024)
                })
                .collect();

            let mut stream = materialize(blocks.clone(), TierPriority::High);
            let mut delivered = 0u64;
            while let Some(chunk) = stream.next().await {
                let buf = chunk.expect("block read failed").copy_to_bytes();
                let block = &blocks[delivered as usize];
                assert_eq!(buf.len(), block.len as usize, "block {delivered} wrong len");
                for (j, b) in buf.iter().enumerate() {
                    let expected = (block.offset.wrapping_add(j as u64) & 0xff) as u8;
                    assert_eq!(*b, expected, "block {delivered} byte {j} out of order");
                }
                delivered += 1;
            }
            assert_eq!(delivered, 10, "did not deliver all blocks");
            info!(delivered, "materialize: all blocks delivered in FIFO order");
        }
        .primary()
        .spawn();
    });
}

/// Mixed resident + device blocks (the storage-service case): a materialize stream interleaving in-memory
/// `Block::Resident(Bytes)` with on-disk `Block::Read(BlockRef)` must deliver them in FIFO order,
/// with the resident bytes spliced in **verbatim** and **bypassing** the device (no credit consumed,
/// no backend read). Odd indices are resident (filled with byte value `0xAA`); even indices are
/// device reads (the bach backend fills `byte j = (offset + j) & 0xff`). The device pool conserving
/// at exactly its capacity at quiescence proves the resident blocks took no credit.
#[test]
fn materialize_mixes_resident_and_device_blocks() {
    use crate::fs::materialize::Block;
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let latency = Latency {
            read: Duration::from_micros(100),
            write: Duration::from_micros(100),
            per_byte_nanos: 0,
        };
        let (_scheduler, devices) = build(vec![iops_device(8)], 1, latency);
        let dev = devices[0].clone();
        async move {
            const N: u64 = 12;
            const RESIDENT: u8 = 0xAA;
            // Even idx → device read at offset `idx*1024`; odd idx → resident 1 KiB of `0xAA`.
            let items: Vec<Block> = (0..N)
                .map(|i| {
                    if i % 2 == 0 {
                        Block::Read(BlockRef::whole(dev.clone(), fd(), i * 1024, 1024))
                    } else {
                        Block::Resident(crate::byte_vec::ByteVec::from(vec![RESIDENT; 1024]))
                    }
                })
                .collect();

            let mut stream = materialize(items, TierPriority::High);
            let mut delivered = 0u64;
            while let Some(chunk) = stream.next().await {
                // The delivered `ByteVec` is chunked; copy to a contiguous view for byte assertions.
                let buf = chunk.expect("block failed").copy_to_bytes();
                assert_eq!(buf.len(), 1024, "block {delivered} wrong len");
                if delivered.is_multiple_of(2) {
                    // Device read: bach fills byte j = (offset + j) & 0xff, offset = delivered*1024.
                    let offset = delivered * 1024;
                    for (j, b) in buf.iter().enumerate() {
                        let expected = (offset.wrapping_add(j as u64) & 0xff) as u8;
                        assert_eq!(*b, expected, "device block {delivered} byte {j} mismatch");
                    }
                } else {
                    assert!(
                        buf.iter().all(|&b| b == RESIDENT),
                        "resident block {delivered} not delivered verbatim"
                    );
                }
                delivered += 1;
            }
            assert_eq!(delivered, N, "did not deliver all mixed blocks in order");
            // Resident blocks consumed no credit; every device read released → pool back to full.
            assert_conserved(&dev);
            info!("materialize: resident+device interleave delivered in order, pool conserved");
        }
        .primary()
        .spawn();
    });
}

/// A materialize stream whose front block is resident delivers that block on the poll that pulls
/// it. `pull` files a resident block `Ready` in the reorder window itself, and no completion wakes
/// the stream for it, so a `Pending` from that poll would never end. One poll with a no-op waker
/// returns the block verbatim, and the next poll ends the stream.
#[test]
fn materialize_delivers_a_resident_front_block_on_its_first_poll() {
    use crate::fs::materialize::Block;
    use core::task::Poll;

    const RESIDENT: u8 = 0x3c;
    let bytes = crate::byte_vec::ByteVec::from(vec![RESIDENT; 1024]);
    let mut stream = materialize([Block::Resident(bytes)], TierPriority::High);
    let waker = s2n_quic_core::task::waker::noop();
    let mut cx = core::task::Context::from_waker(&waker);

    let first = {
        let mut next = core::pin::pin!(stream.next());
        core::future::Future::poll(next.as_mut(), &mut cx)
    };
    match first {
        Poll::Ready(Some(Ok(chunk))) => {
            let buf = chunk.copy_to_bytes();
            assert_eq!(buf.len(), 1024, "the resident block's length changed");
            assert!(
                buf.iter().all(|&b| b == RESIDENT),
                "the resident block isn't delivered verbatim"
            );
        }
        Poll::Ready(Some(Err(error))) => panic!("the first poll returned an error: {error}"),
        Poll::Ready(None) => panic!("the first poll ended the stream before its block"),
        Poll::Pending => panic!("the first poll of a resident block returned Pending"),
    }

    let second = {
        let mut next = core::pin::pin!(stream.next());
        core::future::Future::poll(next.as_mut(), &mut cx)
    };
    assert!(
        matches!(second, Poll::Ready(None)),
        "the second poll ends the stream"
    );
}

/// Cross-device head-of-line blocking: a materialize stream whose blocks alternate between a
/// **credit-starved** device (depth 1) and a roomy one must still make progress and deliver all
/// blocks in FIFO order. With one shared acquire slot, a block parked on the starved device's credit
/// would block submitting the *next* block — even though its device has free credit — and the stream
/// would crawl at the starved device's rate or wedge. The per-device acquire slots let the roomy
/// device's blocks submit while the starved device's block is parked.
#[test]
fn materialize_cross_device_no_hol() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        // Device 0: depth 1 (one op in flight at a time) — the credit-starved device. Device 1:
        // depth 8 — roomy. Equal latency so ordering is driven by submission, not completion time.
        let latency = Latency {
            read: Duration::from_micros(100),
            write: Duration::from_micros(100),
            per_byte_nanos: 0,
        };
        let (_scheduler, devices) = build(vec![iops_device(1), iops_device(8)], 2, latency);
        async move {
            // 20 blocks alternating devices; device 0 (even indices) is depth-1 so its blocks must
            // serialize, while device 1 (odd indices) can have many in flight at once.
            let blocks: Vec<BlockRef> = (0..20u64)
                .map(|i| BlockRef::whole(devices[(i % 2) as usize].clone(), fd(), i * 1024, 1024))
                .collect();

            let mut stream = materialize(blocks.clone(), TierPriority::High);
            let mut delivered = 0u64;
            while let Some(chunk) = stream.next().await {
                let buf = chunk.expect("block read failed").copy_to_bytes();
                let block = &blocks[delivered as usize];
                assert_eq!(buf.len(), block.len as usize, "block {delivered} wrong len");
                for (j, b) in buf.iter().enumerate() {
                    let expected = (block.offset.wrapping_add(j as u64) & 0xff) as u8;
                    assert_eq!(*b, expected, "block {delivered} byte {j} out of order");
                }
                delivered += 1;
            }
            assert_eq!(delivered, 20, "did not deliver all blocks across devices");
            // Conservation across both pools at quiescence. (The blocks above hold clones of these
            // handles, so they remain valid; check each held handle directly.)
            for device in &devices {
                assert_conserved(device);
            }
            info!(
                delivered,
                "materialize: cross-device delivered in order, no HOL wedge"
            );
        }
        .primary()
        .spawn();
    });
}

/// ADVERSARIAL REPRO (case #7): dropping a `MaterializeStream` mid-spray — with some blocks in
/// flight (credit on `op.flow_credits`, awaiting backend completion) and some parked on a starved
/// device's credit — must conserve credit at quiescence. On drop:
///
/// * every `DeviceSlot`'s `SubmitterAlloc` releases any residual `pending_credits` (parked grant),
/// * `ToSubmit` slots hold no credit (acquired only on enqueue), so they release nothing,
/// * in-flight ops still complete in the backend, where `complete()` releases their credit BEFORE
///   discarding the completion onto the now-dead `completion_rx` (receiver_alive == false).
///
/// If `complete()` released after the dead-receiver check, or if a residual alloc grant were
/// abandoned without release, this would leak. Assert the pools return to full capacity.
#[test]
fn materialize_drop_mid_spray_conserves_credit() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        // Non-trivial latency so ops stay in flight across the drop; device 0 is depth-1 (starved,
        // forces parked-on-credit ToSubmit blocks), device 1 is roomy (many in flight).
        let latency = Latency {
            read: Duration::from_micros(500),
            write: Duration::from_micros(500),
            per_byte_nanos: 0,
        };
        let (_scheduler, devices) = build(vec![iops_device(1), iops_device(8)], 2, latency);
        async move {
            // 40 blocks alternating devices: device 0 (even) serializes at depth 1, device 1 (odd)
            // piles up in flight. More blocks than either pool depth, so at the drop point there are
            // both in-flight ops and ToSubmit blocks parked on device 0's credit.
            let blocks: Vec<BlockRef> = (0..40u64)
                .map(|i| BlockRef::whole(devices[(i % 2) as usize].clone(), fd(), i * 1024, 1024))
                .collect();

            let mut stream = materialize(blocks, TierPriority::High);
            // Deliver a few blocks to drive the spray into steady state (ops in flight + parked),
            // then drop the stream mid-flight.
            let mut delivered = 0u64;
            while delivered < 4 {
                match stream.next().await {
                    Some(Ok(_)) => delivered += 1,
                    Some(Err(e)) => panic!("unexpected block error: {e}"),
                    None => break,
                }
            }
            // Prove the drop is genuinely mid-flight: at least one pool has credit out (in-flight
            // ops + parked-on-credit ToSubmit blocks), so this exercises the release-on-late-
            // completion path rather than a quiescent no-op drop.
            let in_flight = devices.iter().any(|device| {
                device
                    .pools
                    .all()
                    .any(|pool| pool.debug_free_total() < pool.debug_capacity() as i64)
            });
            assert!(
                in_flight,
                "test precondition: expected credit in flight at drop point"
            );
            drop(stream);

            // Let any in-flight ops complete (their completions route to the dropped receiver, but
            // the dispatcher releases their credit first) and the dropped allocs release residuals.
            10.ms().sleep().await;

            for device in &devices {
                info!(label = ?device.label, "drop-mid-spray conservation check");
                assert_conserved(device);
            }
            info!("materialize: drop mid-spray conserved credit");
        }
        .primary()
        .spawn();
    });
}

/// A backend completion of `Failed` must surface as `Err` from `read()`/`write()` — never as `Ok`
/// with a bogus buffer.
#[test]
fn failed_completion_surfaces_as_err() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let clock = Clock::default();
        let backend = fail_backend(clock.clone(), std::io::ErrorKind::Other);
        let mut spawn = bach_spawner();
        let scheduler = DeviceRegistry::new(backend, &mut spawn, &Registry::default(), clock);
        let dev = scheduler.register_device("dev0", &iops_device(8)).unwrap();

        async move {
            let read = dev.read(fd(), 0, 4096, TierPriority::Medium).await;
            assert!(read.is_err(), "failed read must surface as Err, got Ok");
            let write = dev
                .write(
                    fd(),
                    0,
                    bytes::Bytes::from_static(b"abcd"),
                    TierPriority::Medium,
                )
                .await;
            assert!(write.is_err(), "failed write must surface as Err, got Ok");
            assert_conserved(&dev);
            info!("failed completion surfaced as Err and conserved credit");
        }
        .primary()
        .spawn();
    });
}

/// An op whose credit cost exceeds its device pool capacity must fail fast with `Err`, not park
/// forever (an atomic `IoOp` has no partial-submit escape).
#[test]
fn oversize_op_fails_fast() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let (_scheduler, devices) = build(vec![iops_device(4)], 1, Latency::default());
        let dev = devices[0].clone();
        async move {
            let result = dev.read(fd(), 0, 64 * 1024, TierPriority::Medium).await;
            assert!(
                matches!(
                    result.as_ref().map_err(|e| e.kind()),
                    Err(std::io::ErrorKind::InvalidInput)
                ),
                "oversize op must fail fast with InvalidInput, got {result:?}"
            );
            info!("oversize op rejected fast");
        }
        .primary()
        .spawn();
    });
}

/// An op whose submitter dropped its completion receiver **before** the op reached execution must be
/// **skipped** by the backend — the syscall/SQE/processing is never issued (wasted device work
/// avoided) — while its borrowed credit is still released (conservation holds).
///
/// We prove the skip behaviorally: a backend processor increments a shared counter every time it
/// actually runs an op. We submit one op against a device whose latency keeps it parked in the
/// per-op timer task, drop the completion receiver while it waits, advance simulated time past the
/// latency, and assert (1) the processor never ran and (2) the pool returned to full capacity.
#[test]
fn cancelled_op_skips_execution_and_conserves_credit() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        // A processor that records every op it actually executes. If the cancel-skip works, it never
        // fires for the dropped op.
        let ran = Rc::new(std::cell::Cell::new(0u64));
        let ran_proc = ran.clone();

        let clock = Clock::default();
        // Long latency so the op is still parked in its per-op timer task when we drop the receiver.
        let latency = Latency {
            read: Duration::from_millis(10),
            write: Duration::from_millis(10),
            per_byte_nanos: 0,
        };
        let backend = BachBackend::with_processor(clock.clone(), latency, move |op: &mut IoOp| {
            ran_proc.set(ran_proc.get() + 1);
            crate::fs::backend::bach::process_default(op);
        });
        let mut spawn = bach_spawner();
        let scheduler = DeviceRegistry::new(backend, &mut spawn, &Registry::default(), clock);
        let dev = scheduler.register_device("dev0", &iops_device(8)).unwrap();

        async move {
            // Enqueue one read against a completion receiver we own, then drop the receiver before the
            // 10ms latency elapses. `Reservation::enqueue` enqueues the op (credit acquired) without
            // awaiting completion — the non-await half of the two-phase submit — so we can drop the
            // receiver mid-flight.
            let completion_rx =
                crate::socket::channel::intrusive::datagram_completion::new::<IoOp>();
            dev.reserve(
                crate::fs::op::IoKind::Read,
                fd(),
                0,
                4096,
                TierPriority::Medium,
                false,
            )
            .await
            .expect("reserve should acquire credit")
            .enqueue(
                crate::fs::op::IoBuf::Read(bytes::BytesMut::with_capacity(4096)),
                completion_rx.sender(),
                0,
            )
            .expect("enqueue should admit the op");

            // Credit is out (op enqueued, latency not yet elapsed): this is the genuine mid-flight
            // drop the skip path is for.
            assert!(
                dev.pools
                    .all()
                    .any(|pool| pool.debug_free_total() < pool.debug_capacity() as i64),
                "precondition: op should hold credit before cancellation"
            );

            // Cancel: drop the receiver so `receiver_alive()` flips false before the op runs.
            drop(completion_rx);

            // Advance past the latency: the per-op task wakes, observes the dropped receiver, and
            // skips execution via `complete_cancelled` instead of running the processor.
            50.ms().sleep().await;

            assert_eq!(
                ran.get(),
                0,
                "cancelled op must NOT have been executed by the backend"
            );
            assert_conserved(&dev);
            info!("cancelled op skipped execution and conserved credit");
        }
        .primary()
        .spawn();
    });
}

/// The per-op flight recorder ([`crate::fs::trace`]) must fire the full healthy lifecycle for a normal
/// op: `Submitted` → `Dispatched` → `BackendStart` → `BackendDone` → `Completed`. We drive one read
/// through the bach backend and sample the global recorder afterwards (tests have the recorder armed
/// via `init_tracing`). Asserting *containment* keeps the test robust to the process-global ring
/// accumulating rows from other tests.
#[test]
fn trace_records_full_lifecycle_for_a_normal_op() {
    use crate::fs::trace::IoLifecycle;
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let (_scheduler, devices) = build(vec![iops_device(8)], 1, Latency::default());
        let dev = devices[0].clone();
        async move {
            let _scheduler = _scheduler;
            let buf = dev
                .read(fd(), 0, 4096, TierPriority::Medium)
                .await
                .expect("read should complete");
            assert_eq!(buf.len(), 4096, "bach backend fills the requested length");
            // Let the completion settle before sampling the ring.
            10.ms().sleep().await;

            let (lifecycles, kinds) = crate::fs::trace::resident_event_kinds();
            for lc in [
                IoLifecycle::Submitted,
                IoLifecycle::Dispatched,
                IoLifecycle::BackendStart,
                IoLifecycle::BackendDone,
                IoLifecycle::Completed,
            ] {
                assert!(
                    lifecycles.contains(&lc.as_u8()),
                    "expected a {lc:?} io_op record in the ring; saw lifecycles {lifecycles:?}"
                );
            }
            assert!(
                kinds.contains(&crate::fs::trace::IoOpKind::Read.as_u8()),
                "expected a Read-kind io_op record; saw kinds {kinds:?}"
            );
            info!("trace recorded the full normal-op lifecycle");
        }
        .primary()
        .spawn();
    });
}

/// A pre-admission rejection (`Device::prepare` fails on an oversized op) must fire the
/// [`Rejected`](crate::fs::trace::IoLifecycle::Rejected) trace point — the row that has no `op_seq`
/// because no `IoOp` was ever built.
#[test]
fn trace_records_rejected_for_oversize_op() {
    use crate::fs::trace::IoLifecycle;
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let (_scheduler, devices) = build(vec![iops_device(4)], 1, Latency::default());
        let dev = devices[0].clone();
        async move {
            let _scheduler = _scheduler;
            let result = dev.read(fd(), 0, 64 * 1024, TierPriority::Medium).await;
            assert!(result.is_err(), "oversize op must be rejected");
            let (lifecycles, _kinds) = crate::fs::trace::resident_event_kinds();
            assert!(
                lifecycles.contains(&IoLifecycle::Rejected.as_u8()),
                "expected a Rejected io_op record; saw lifecycles {lifecycles:?}"
            );
            info!("trace recorded the rejection");
        }
        .primary()
        .spawn();
    });
}

/// A submitter that parks on credit (depth-1 device with a long-latency occupant) must fire
/// [`CreditPark`](crate::fs::trace::IoLifecycle::CreditPark), and the eventual grant must fire
/// [`CreditGrant`](crate::fs::trace::IoLifecycle::CreditGrant). This exercises the credit-acquire trace
/// points wired through `SubmitterAlloc`.
#[test]
fn trace_records_credit_park_and_grant() {
    use crate::fs::trace::IoLifecycle;
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        // depth-1 device, 50ms latency: the first op saturates the pool and stays in flight, so the
        // second op must park on credit and is granted only when the first completes.
        let latency = Latency {
            read: Duration::from_millis(50),
            write: Duration::from_millis(50),
            per_byte_nanos: 0,
        };
        let (_scheduler, devices) = build(vec![iops_device(1)], 1, latency);
        let dev = devices[0].clone();
        async move {
            let _scheduler = _scheduler;
            // Spawn two concurrent reads: the first holds the only credit for 50ms, forcing the second
            // to park on acquire and be granted on the first's release.
            let dev_a = dev.clone();
            async move {
                dev_a
                    .read(fd(), 0, 4096, TierPriority::Medium)
                    .await
                    .expect("first read completes");
            }
            .spawn();
            let dev_b = dev.clone();
            async move {
                dev_b
                    .read(fd(), 4096, 4096, TierPriority::Medium)
                    .await
                    .expect("second read completes");
            }
            .spawn();

            // Both reads (50ms each, serialized on the depth-1 pool) finish well within this window.
            200.ms().sleep().await;

            let (lifecycles, _kinds) = crate::fs::trace::resident_event_kinds();
            for lc in [IoLifecycle::CreditPark, IoLifecycle::CreditGrant] {
                assert!(
                    lifecycles.contains(&lc.as_u8()),
                    "expected a {lc:?} io_op record; saw lifecycles {lifecycles:?}"
                );
            }
            info!("trace recorded credit park and grant");
        }
        .primary()
        .spawn();
    });
}

/// Every op a single `materialize` run enqueues must carry the **same** `stream_id`, so a dump can
/// group the whole streamed read (`WHERE stream_id = N`). We run one stream of 6 device blocks and
/// assert the recorder holds exactly one stream id, carrying multiple lifecycle rows (≥ one per block's
/// `Submitted`). One-off `submit`s in other tests carry `NO_STREAM` and never show up here.
#[test]
fn trace_groups_materialize_ops_under_one_stream_id() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let latency = Latency {
            read: Duration::from_micros(100),
            write: Duration::from_micros(100),
            per_byte_nanos: 0,
        };
        let (_scheduler, devices) = build(vec![iops_device(8), iops_device(8)], 2, latency);
        async move {
            let _scheduler = _scheduler;
            let blocks: Vec<BlockRef> = (0..6u64)
                .map(|i| {
                    let dev = devices[(i % 2) as usize].clone();
                    BlockRef::whole(dev, fd(), i * 1024, 1024)
                })
                .collect();
            let mut stream = materialize(blocks, TierPriority::High);
            // This run's own id — assert on it directly rather than diffing the process-global recorder
            // (other tests may record their own stream ids concurrently, so a before/after diff is racy).
            let sid = stream.stream_id();
            let mut delivered = 0u64;
            while let Some(chunk) = stream.next().await {
                chunk.expect("block read failed");
                delivered += 1;
            }
            assert_eq!(delivered, 6, "all blocks delivered");
            10.ms().sleep().await;

            // Our stream id groups at least one row per block (>= 6: Submitted/Dispatched/Backend*/
            // Completed all carry it). Keyed on `sid`, so concurrent tests' ids are irrelevant.
            let resident = crate::fs::trace::resident_stream_ids();
            let count = resident.get(&sid).copied().unwrap_or(0);
            assert!(
                count >= 6,
                "stream id {sid} must group every block's ops (>= 6 rows); saw {count}"
            );
            info!(
                stream_id = sid,
                count, "materialize ops grouped under one stream id"
            );
        }
        .primary()
        .spawn();
    });
}

/// REGRESSION: a registered device's credit pool(s) must record their counters into the registry
/// the scheduler was handed — **not** a private, undrained one.
///
/// `Device::new` used to build its pools with `Pool::new`, which substitutes `Counters::default()`
/// (a private `Registry` no reporter ever drains). The always-spinning `Distributor::pass()` records
/// `distributor.passes` into that registry millions of times/sec, and the rseq recorder mints a fresh
/// 64 KiB `Page` per event that is only recycled when a reporter drains the registry — so an undrained
/// registry leaks pages without bound (~GB/min per host → OOM). The fix wires the pools to the
/// passed-in registry via `Pool::with_counters` under the same `fs.credit.{label}.pool{idx}` prefix
/// the gauges use.
///
/// We drive one read through a device registered against a real registry, then drain that registry
/// and assert the pool's acquire counter appears under the expected prefix. Before the fix the pool
/// recorded into its private registry, so this counter would be absent from the drained line.
#[test]
fn device_credit_pool_records_into_passed_in_registry() {
    let _no_snap = crate::testing::without_snapshots();
    sim(|| {
        let clock = Clock::default();
        let backend = BachBackend::new(clock.clone(), Latency::default());
        let mut spawn = bach_spawner();
        let registry = Registry::new();
        let scheduler = DeviceRegistry::new(backend, &mut spawn, &registry, clock);
        let dev = scheduler.register_device("dev0", &iops_device(8)).unwrap();

        async move {
            let _scheduler = scheduler;
            // A successful read takes the pool's fast-path acquire, bumping `acquire.fast_path` /
            // `acquire.bytes` on the pool's counters.
            dev.read(fd(), 0, 4096, TierPriority::Medium)
                .await
                .expect("read should complete");
            10.ms().sleep().await;

            // Drain the registry we handed the scheduler. The pool's acquire counter must be present
            // under the device's gauge prefix — proving it recorded here, not into a private registry.
            let line = registry
                .take_current_metrics_line()
                .expect("registry should have recorded at least one metric");
            assert!(
                line.contains("fs.credit.dev0.pool0.acquire.fast_path"),
                "device pool must record its counters into the passed-in registry under \
                 `fs.credit.dev0.pool0.*`; metrics line was: {line}"
            );
            info!("device credit pool records into the passed-in registry");
        }
        .primary()
        .spawn();
    });
}
