// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{
    byte_vec::ByteVec,
    endpoint::{
        frame::{Header, TransmissionStatus, DEFAULT_TTL},
        id::{Id, LocalSendSocketId, LocalSenderId},
    },
    path::secret::map::Entry as PathSecretEntry,
    socket::channel::{intrusive::unsync, ByteCost},
    time::testing as test_clock_mod,
};
use bytes::{Bytes, BytesMut};
use core::task::Poll;
use s2n_quic_core::varint::VarInt;
use std::{
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn test_clock() -> test_clock_mod::Clock {
    test_clock_mod::Clock::new(std::time::Duration::from_secs(1))
}

fn test_completion_dispatcher<R>(rx: R) -> CompletionDispatcher<R>
where
    R: Receiver<Entry<Frame>>,
{
    let registry = crate::counter::Registry::default();
    let clock = crate::time::DefaultClock::default();
    let reader_metrics = Arc::new(crate::stream::metrics::ReaderMetrics::new(
        &registry,
        "test.reader",
    ));
    let writer_metrics = Arc::new(crate::stream::metrics::WriterMetrics::new(
        &registry,
        "test.writer",
    ));
    let send_credit_pool = crate::sync::Arc::new(crate::credit::Pool::new(
        crate::credit::Config::new(1_000_000),
    ));
    CompletionDispatcher::new(rx, clock, reader_metrics, writer_metrics, send_credit_pool)
}

struct TestItem {
    path_secret_entry: Arc<PathSecretEntry>,
    byte_cost: u64,
    flow_credits: u64,
    drop_counter: Arc<AtomicUsize>,
}

impl Drop for TestItem {
    fn drop(&mut self) {
        self.drop_counter.fetch_add(1, Ordering::Relaxed);
    }
}

impl ByteCost for TestItem {
    fn byte_cost(&self) -> u64 {
        self.byte_cost
    }
}

impl PathSecretMapEntry for TestItem {
    fn path_secret_entry(&self) -> &Arc<PathSecretEntry> {
        &self.path_secret_entry
    }
}

impl AssignSender for TestItem {
    fn set_sender_id(&mut self, _id: crate::endpoint::id::LocalSenderId) {}
}

impl FlowCredits for TestItem {
    fn total_flow_credits(&self) -> u64 {
        self.flow_credits
    }
}

struct TestSender {
    accept: bool,
    calls: usize,
}

impl UnboundedSender<TestItem> for TestSender {
    fn send(&mut self, value: TestItem) -> Result<(), TestItem> {
        self.calls += 1;
        if self.accept {
            drop(value);
            Ok(())
        } else {
            Err(value)
        }
    }
}

struct TestReceiver<T> {
    values: VecDeque<T>,
    consumed: u64,
}

impl<T> Receiver<T> for TestReceiver<T> {
    fn poll_recv(&mut self, _cx: &mut task::Context<'_>, _budget: &mut Budget) -> Poll<Option<T>> {
        Poll::Ready(self.values.pop_front())
    }

    fn on_consumed(&mut self, bytes: u64) {
        self.consumed += bytes;
    }
}

struct BudgetAwareTestReceiver<T> {
    values: VecDeque<T>,
}

impl<T> Receiver<T> for BudgetAwareTestReceiver<T> {
    fn poll_recv(&mut self, _cx: &mut task::Context<'_>, budget: &mut Budget) -> Poll<Option<T>> {
        if budget.is_exhausted() {
            budget.set_needs_wake();
            return Poll::Pending;
        }

        match self.values.pop_front() {
            Some(value) => {
                budget.consume();
                Poll::Ready(Some(value))
            }
            None => Poll::Ready(None),
        }
    }

    fn on_consumed(&mut self, _bytes: u64) {}
}

fn test_path_secret_entry() -> Arc<PathSecretEntry> {
    let peer: std::net::SocketAddr = "127.0.0.1:4433".parse().unwrap();
    PathSecretEntry::builder(peer)
        .socket_sender_count(2)
        .build(None)
}

fn new_test_item(
    path_secret_entry: Arc<PathSecretEntry>,
    drop_counter: Arc<AtomicUsize>,
) -> TestItem {
    TestItem {
        path_secret_entry,
        byte_cost: 123,
        flow_credits: 0,
        drop_counter,
    }
}

fn new_test_frame(path_secret_entry: Arc<PathSecretEntry>, payload_len: usize) -> Entry<Frame> {
    new_test_frame_with_header(
        path_secret_entry,
        payload_len,
        Header::QueueControl {
            queue_pair: crate::packet::datagram::QueuePair {
                source_queue_id: VarInt::from_u8(0),
                dest_queue_id: VarInt::from_u8(1),
            },
            binding_id: VarInt::from_u8(0),
        },
    )
}

fn new_test_frame_with_header(
    path_secret_entry: Arc<PathSecretEntry>,
    payload_len: usize,
    header: Header,
) -> Entry<Frame> {
    let mut payload = ByteVec::new();
    if payload_len > 0 {
        payload.push_back(Bytes::from(vec![0u8; payload_len]));
    }

    Entry::new(Frame {
        header,
        payload,
        path_secret_entry,
        completion: None,
        status: TransmissionStatus::Pending,
        ttl: DEFAULT_TTL,
        enqueued_at: None,
        flow_credits: 0,
    })
}

fn drive_completion_dispatcher(
    dispatcher: &mut CompletionDispatcher<TestReceiver<Entry<Frame>>>,
    budget_capacity: usize,
) -> Poll<Option<crate::queue::AutoWake>> {
    with_noop_context(|cx| {
        let mut budget = Budget::new(budget_capacity);
        dispatcher.poll_recv(cx, &mut budget)
    })
}

fn with_noop_context<R>(f: impl FnOnce(&mut task::Context<'_>) -> R) -> R {
    let waker = s2n_quic_core::task::waker::noop();
    let mut cx = task::Context::from_waker(&waker);
    f(&mut cx)
}

// ── MappedSender tests ─────────────────────────────────────────────────────

#[derive(Debug)]
struct MappedItem {
    sender_id: LocalSenderId,
    value: usize,
}

impl HasId<LocalSenderId> for MappedItem {
    fn id(&self) -> LocalSenderId {
        self.sender_id
    }
}

struct MappedItemSender {
    sink: Rc<RefCell<Vec<usize>>>,
}

impl UnboundedSender<MappedItem> for MappedItemSender {
    fn send(&mut self, value: MappedItem) -> Result<(), MappedItem> {
        self.sink.borrow_mut().push(value.value);
        Ok(())
    }
}

#[test]
fn mapped_sender_routes_items_through_id_map() {
    let sink0 = Rc::new(RefCell::new(Vec::new()));
    let sink1 = Rc::new(RefCell::new(Vec::new()));

    let senders: crate::endpoint::id::IdMap<LocalSendSocketId, MappedItemSender> = [
        (
            LocalSendSocketId::new(0),
            MappedItemSender {
                sink: sink0.clone(),
            },
        ),
        (
            LocalSendSocketId::new(1),
            MappedItemSender {
                sink: sink1.clone(),
            },
        ),
    ]
    .into_iter()
    .collect();

    let mut sender_idx_to_local: crate::endpoint::id::IdMap<LocalSenderId, LocalSendSocketId> =
        crate::endpoint::id::IdMap::new(2, LocalSendSocketId::new(usize::MAX));
    sender_idx_to_local[LocalSenderId::from_index(0)] = LocalSendSocketId::new(1);
    sender_idx_to_local[LocalSenderId::from_index(1)] = LocalSendSocketId::new(0);

    let mut tx = MappedSender::new(senders, sender_idx_to_local);
    tx.send(MappedItem {
        sender_id: LocalSenderId::from_index(0),
        value: 10,
    })
    .unwrap();
    tx.send(MappedItem {
        sender_id: LocalSenderId::from_index(1),
        value: 20,
    })
    .unwrap();

    assert_eq!(&*sink0.borrow(), &[20]);
    assert_eq!(&*sink1.borrow(), &[10]);
}

// ── PickTwo tests ─────────────────────────────────────────────────────────

fn try_send_pick_two(
    value: TestItem,
    senders: &mut Vec<TestSender>,
    rng: &mut crate::xorshift::Rng,
) -> Result<(), TestItem> {
    try_send_pick_two_with_rr(value, senders, rng, &mut 0)
}

fn try_send_pick_two_with_rr(
    value: TestItem,
    senders: &mut Vec<TestSender>,
    rng: &mut crate::xorshift::Rng,
    round_robin_idx: &mut usize,
) -> Result<(), TestItem> {
    let registry = crate::counter::Registry::default();
    let pick_counters: Vec<_> = (0..senders.len())
        .map(|i| registry.register_nominal("pick_two.chosen", format_args!("send.{i}")))
        .collect();
    let rejected_counters: Vec<_> = (0..senders.len())
        .map(|i| {
            registry.register_nominal_summary(
                "pick_two.rejected",
                format_args!("send.{i}"),
                crate::counter::Unit::Microsecond,
            )
        })
        .collect();
    let score_delta =
        registry.register_summary("pick_two.score_delta", crate::counter::Unit::Microsecond);
    let pick_counters_map: crate::endpoint::id::IdMap<crate::endpoint::id::LocalSenderId, _> =
        pick_counters.into();
    let rejected_counters_map: crate::endpoint::id::IdMap<crate::endpoint::id::LocalSenderId, _> =
        rejected_counters.into();
    let mut senders_map: crate::endpoint::id::IdMap<crate::endpoint::id::LocalSenderId, _> =
        std::mem::take(senders).into();
    let result = PickTwo::<TestItem, TestReceiver<TestItem>, TestSender>::try_send_pick_two(
        value,
        &mut senders_map,
        rng,
        round_robin_idx,
        &pick_counters_map,
        &rejected_counters_map,
        &score_delta,
    );
    *senders = senders_map.into_iter().map(|(_, v)| v).collect();
    result
}

#[test]
fn selected_sender_receives_item() {
    let item = new_test_item(test_path_secret_entry(), Arc::new(AtomicUsize::new(0)));
    let mut senders = vec![
        TestSender {
            accept: true,
            calls: 0,
        },
        TestSender {
            accept: true,
            calls: 0,
        },
    ];
    let result = try_send_pick_two(item, &mut senders, &mut crate::xorshift::Rng::new());
    assert!(result.is_ok());
    assert_eq!(senders[0].calls + senders[1].calls, 1);
}

#[test]
fn sender_error_returns_value() {
    let drop_counter = Arc::new(AtomicUsize::new(0));
    let item = new_test_item(test_path_secret_entry(), drop_counter.clone());
    let mut senders = vec![
        TestSender {
            accept: false,
            calls: 0,
        },
        TestSender {
            accept: false,
            calls: 0,
        },
    ];
    let result = try_send_pick_two(item, &mut senders, &mut crate::xorshift::Rng::new());
    assert!(result.is_err());
    assert_eq!(senders[0].calls + senders[1].calls, 1);
    assert_eq!(drop_counter.load(Ordering::Relaxed), 0);

    drop(result);
    assert_eq!(drop_counter.load(Ordering::Relaxed), 1);
}

/// Set sender `idx`'s published load score to exactly `score_nanos` by feeding a base timestamp
/// of that many nanoseconds with zero queued bytes (so the queue-drain term is zero and the stored
/// score equals the base).
///
/// `score_nanos` must be a whole number of microseconds: the core `Timestamp` stores microseconds
/// internally and truncates sub-microsecond nanos (see `score_as_u64`), so a value like `1_500` ns
/// would round down to `1_000` ns. Callers here pass millisecond-scale gaps, so this is exact.
fn set_load_score(entry: &Arc<PathSecretEntry>, idx: usize, score_nanos: u64) {
    debug_assert_eq!(
        score_nanos % 1_000,
        0,
        "score resolution is one microsecond"
    );
    let base = unsafe {
        s2n_quic_core::time::Timestamp::from_duration(core::time::Duration::from_nanos(score_nanos))
    };
    entry.update_sender_load_score(
        LocalSenderId::from_index(idx),
        base,
        0,
        s2n_quic_core::recovery::bandwidth::Bandwidth::new(
            1_000,
            core::time::Duration::from_millis(1),
        ),
    );
}

/// Run `iters` pick-two routings between two senders with the given load-score gap and return the
/// number of times the higher-scored (worse) sender was chosen. Sender 0 is the better candidate
/// (score 0); sender 1 is worse (score `gap_nanos`).
fn count_worse_picks(gap_nanos: u64, iters: usize, seed: u64) -> usize {
    let entry = test_path_secret_entry();
    set_load_score(&entry, 0, 0);
    set_load_score(&entry, 1, gap_nanos);

    let mut rng = crate::xorshift::Rng::with_seed(seed);
    let mut rr = 0;
    let mut worse = 0;
    for _ in 0..iters {
        let mut senders = vec![
            TestSender {
                accept: true,
                calls: 0,
            },
            TestSender {
                accept: true,
                calls: 0,
            },
        ];
        let item = new_test_item(entry.clone(), Arc::new(AtomicUsize::new(0)));
        assert!(try_send_pick_two_with_rr(item, &mut senders, &mut rng, &mut rr).is_ok());
        // Sender 1 is the worse (higher-score) candidate.
        worse += senders[1].calls;
    }
    worse
}

/// `PICK_TWO_FLOOR_LN_RATIO` is a hand-pinned literal because `f64::ln` is not const-stable. This
/// guards it against drifting out of sync with the floor — change the floor without updating the
/// literal and this fails instead of silently skewing the curve.
#[test]
fn pick_two_worse_probability_floor_ratio_literal() {
    let expected = ((1.0 - PICK_TWO_WORSE_FLOOR) / PICK_TWO_WORSE_FLOOR).ln();
    assert!(
        (PICK_TWO_FLOOR_LN_RATIO - expected).abs() < 1e-12,
        "PICK_TWO_FLOOR_LN_RATIO={PICK_TWO_FLOOR_LN_RATIO} is stale; expected ln((1-floor)/floor)={expected}"
    );
}

#[test]
fn pick_two_worse_probability_curve() {
    // Equal scores must be a fair coin flip (no structural bias toward idx1 or idx2).
    assert_eq!(pick_two_worse_probability(0), 0.5);

    // The curve is derived so the logistic meets the floor *exactly* at the worst-case gap — the
    // smooth decay and the clamp join with no knee.
    let worst_case = PICK_TWO_WORST_CASE_GAP_NANOS as u64;
    let p_worst = pick_two_worse_probability(worst_case);
    assert!(
        (p_worst - PICK_TWO_WORSE_FLOOR).abs() < 1e-6,
        "expected logistic to equal the floor {PICK_TWO_WORSE_FLOOR} at the worst-case gap, got {p_worst}"
    );

    // Intermediate gaps follow the derived logistic: ~9% at half the worst-case gap.
    let p_half = pick_two_worse_probability(worst_case / 2);
    assert!(
        (0.08..0.10).contains(&p_half),
        "expected ~0.09 at half the worst-case gap, got {p_half}"
    );

    // Probability is monotonically non-increasing as the gap widens.
    assert!(pick_two_worse_probability(2 * worst_case) <= p_worst);

    // A huge gap clamps to the floor rather than collapsing to zero — this is what prevents a
    // structurally-worse target from ever being fully starved.
    assert_eq!(pick_two_worse_probability(u64::MAX), PICK_TWO_WORSE_FLOOR);
}

/// Regression test for the starvation problem with the old deterministic `score1 <= score2` rule:
/// a sender that is consistently worse received *zero* traffic. With the probabilistic rule it must
/// keep getting a probe trickle at roughly the floor rate, never zero.
#[test]
fn consistently_worse_sender_is_not_starved() {
    const ITERS: usize = 20_000;
    // A gap far larger than the worst-case gap: the logistic term is effectively zero here, so any
    // traffic the worse sender receives comes from the floor. The old deterministic rule routed 0.
    let worse = count_worse_picks(
        1_000 * PICK_TWO_WORST_CASE_GAP_NANOS as u64,
        ITERS,
        0x5eed_1234,
    );

    let share = worse as f64 / ITERS as f64;
    assert!(
        worse > 0,
        "consistently-worse sender was fully starved (the bug this fix addresses)"
    );
    // Should hover around the floor (1%). Allow a statistical band around it.
    assert!(
        (0.005..0.015).contains(&share),
        "worse-sender share {share} should be near the {PICK_TWO_WORSE_FLOOR} floor"
    );
}

/// With equal load scores the two candidates must split traffic roughly evenly.
#[test]
fn equal_scores_split_evenly() {
    const ITERS: usize = 20_000;
    let worse = count_worse_picks(0, ITERS, 0xabcd_9876);
    let share = worse as f64 / ITERS as f64;
    assert!(
        (0.45..0.55).contains(&share),
        "equal-score split {share} should be ~0.5"
    );
}

#[test]
fn pick_two_drops_unsent_entry_on_shutdown() {
    const CAP: u64 = 1_000_000;
    const CREDIT: u64 = 4096;

    let drop_counter = Arc::new(AtomicUsize::new(0));
    // The dropped item carries borrowed send-pool credit; PickTwo must return it on the
    // undeliverable path or it leaks (Frame has no Drop that releases flow_credits).
    let pool = crate::sync::Arc::new(crate::credit::Pool::new(crate::credit::Config::new(CAP)));
    let mut item = new_test_item(test_path_secret_entry(), drop_counter.clone());
    item.flow_credits = CREDIT;
    let rx = TestReceiver {
        values: [item].into(),
        consumed: 0,
    };
    let senders = vec![
        TestSender {
            accept: false,
            calls: 0,
        },
        TestSender {
            accept: false,
            calls: 0,
        },
    ];
    let registry = crate::counter::Registry::default();
    let pick_two = PickTwo::new(
        rx,
        senders.into(),
        crate::xorshift::Rng::new(),
        &registry,
        pool.clone(),
    );
    let mut fut = core::pin::pin!(crate::socket::channel::ReceiverExt::drain_budgeted(
        pick_two, None
    ));
    let result = with_noop_context(|cx| fut.as_mut().poll(cx));
    assert_eq!(result, Poll::Ready(()));
    assert_eq!(drop_counter.load(Ordering::Relaxed), 1);
    // The batch's borrowed credit was released back to the pool when it was dropped. The pool
    // started full (CAP) and the item's `flow_credits` were fabricated (not drained from the
    // pool), so a release shows up as the balance rising to `CAP + CREDIT`. The point is that
    // a release happened at all — the undeliverable-drop path no longer leaks.
    assert_eq!(
        pool.debug_available() as u64 + pool.debug_returned(),
        CAP + CREDIT,
        "PickTwo did not release flow_credits on the undeliverable-batch drop path",
    );
}

/// Regression test: PickTwo must call `on_consumed` on the inner receiver after successfully
/// dispatching an item.  Before the fix, `on_consumed` was never called, so the `Paced`
/// combinator's token bucket was never consumed and pacing was completely bypassed.
#[test]
fn pick_two_propagates_on_consumed() {
    let drop_counter = Arc::new(AtomicUsize::new(0));
    let item = new_test_item(test_path_secret_entry(), drop_counter);
    let expected_byte_cost = item.byte_cost();
    let rx = TestReceiver {
        values: VecDeque::from([item]),
        consumed: 0,
    };
    let senders = vec![
        TestSender {
            accept: true,
            calls: 0,
        },
        TestSender {
            accept: true,
            calls: 0,
        },
    ];
    let registry = crate::counter::Registry::default();
    let pool = crate::sync::Arc::new(crate::credit::Pool::new(crate::credit::Config::new(
        1_000_000,
    )));
    let mut pick_two = PickTwo::new(
        rx,
        senders.into(),
        crate::xorshift::Rng::new(),
        &registry,
        pool,
    );

    let result = with_noop_context(|cx| {
        let mut budget = Budget::new(usize::MAX);
        pick_two.poll_recv(cx, &mut budget)
    });
    assert_eq!(result, Poll::Ready(Some(())));

    // The inner receiver's on_consumed must be called with the item's byte_cost so that
    // upstream Paced combinators advance their token buckets.
    assert_eq!(
        pick_two.rx.consumed, expected_byte_cost,
        "PickTwo must propagate on_consumed(byte_cost) to inner receiver after dispatch"
    );
}

#[test]
fn completion_dispatcher_filters_non_failures_for_failure_only_subscriptions() {
    let path_secret_entry = test_path_secret_entry();
    let mut completion_rx = frame::failure_completion_channel();

    let mut frame = new_test_frame(path_secret_entry, 1).into_inner();
    frame.status = TransmissionStatus::Acknowledged;
    frame.completion = Some(completion_rx.sender());

    let rx = TestReceiver {
        values: [Entry::new(frame)].into(),
        consumed: 0,
    };
    let mut dispatcher = test_completion_dispatcher(rx);
    let _ = drive_completion_dispatcher(&mut dispatcher, usize::MAX);

    with_noop_context(|cx| {
        let result = completion_rx.poll_swap(cx);
        assert!(
            matches!(result, Poll::Pending),
            "acknowledged frame should be filtered before notification"
        );
    });
}

#[test]
fn completion_dispatcher_notifies_failures_for_failure_only_subscriptions() {
    let path_secret_entry = test_path_secret_entry();
    let mut completion_rx = frame::failure_completion_channel();

    let mut frame = new_test_frame(path_secret_entry, 1).into_inner();
    frame.status = TransmissionStatus::Failed(frame::FailureReason::TransmissionError);
    frame.completion = Some(completion_rx.sender());

    let rx = TestReceiver {
        values: [Entry::new(frame)].into(),
        consumed: 0,
    };
    let mut dispatcher = test_completion_dispatcher(rx);
    let _ = drive_completion_dispatcher(&mut dispatcher, usize::MAX);

    with_noop_context(|cx| match completion_rx.poll_swap(cx) {
        Poll::Ready(Some(queue)) => {
            assert_eq!(queue.len(), 1);
            assert!(matches!(
                queue.front().unwrap().status,
                TransmissionStatus::Failed(frame::FailureReason::TransmissionError)
            ));
        }
        other => panic!("expected failed completion notification, got {other:?}"),
    });
}

#[test]
fn completion_dispatcher_polls_past_filtered_frames_in_same_poll() {
    let path_secret_entry = test_path_secret_entry();
    let mut completion_rx = frame::failure_completion_channel();
    let sender = completion_rx.sender();

    let mut acknowledged = new_test_frame(path_secret_entry.clone(), 1).into_inner();
    acknowledged.status = TransmissionStatus::Acknowledged;
    acknowledged.completion = Some(sender.clone());

    let mut failed = new_test_frame(path_secret_entry, 1).into_inner();
    failed.status = TransmissionStatus::Failed(frame::FailureReason::TransmissionError);
    failed.completion = Some(sender);

    let rx = TestReceiver {
        values: [Entry::new(acknowledged), Entry::new(failed)].into(),
        consumed: 0,
    };
    let mut dispatcher = test_completion_dispatcher(rx);

    let _ = drive_completion_dispatcher(&mut dispatcher, usize::MAX);

    with_noop_context(|cx| match completion_rx.poll_swap(cx) {
        Poll::Ready(Some(queue)) => {
            assert_eq!(queue.len(), 1);
            assert!(matches!(
                queue.front().unwrap().status,
                TransmissionStatus::Failed(frame::FailureReason::TransmissionError)
            ));
        }
        other => panic!("expected failed completion notification, got {other:?}"),
    });
}

#[test]
fn completion_dispatcher_returns_pending_when_budget_exhausted_while_filtering() {
    let path_secret_entry = test_path_secret_entry();
    let completion_rx = frame::failure_completion_channel();

    let mut frame = new_test_frame(path_secret_entry, 1).into_inner();
    frame.status = TransmissionStatus::Acknowledged;
    frame.completion = Some(completion_rx.sender());

    let rx = BudgetAwareTestReceiver {
        values: [Entry::new(frame)].into(),
    };
    let mut dispatcher = test_completion_dispatcher(rx);

    with_noop_context(|cx| {
        let mut budget = Budget::new(1);
        let result = dispatcher.poll_recv(cx, &mut budget);
        assert!(matches!(result, Poll::Pending));
        assert!(budget.take_needs_wake());
    });
}

// ── BatchFramesByPathSecret tests ─────────────────────────────────────────

#[test]
fn frame_batch_tracks_byte_costs_per_priority() {
    let path = test_path_secret_entry();
    let first = new_test_frame(path.clone(), 16);
    let first_cost = first.byte_cost();
    let mut batch = FrameBatch::new(first);

    let data = new_test_frame_with_header(
        path.clone(),
        24,
        Header::QueueData {
            queue_pair: crate::packet::datagram::QueuePair {
                source_queue_id: VarInt::from_u8(0),
                dest_queue_id: VarInt::from_u8(1),
            },
            binding_id: VarInt::from_u8(0),
            offset: VarInt::ZERO,
            largest_offset: VarInt::ZERO,
            is_fin: false,
            blocked: false,
            dest_acceptor_id: None,
            priority: crate::credit::Priority::default(),
        },
    );
    let data_cost = data.byte_cost();
    batch.push_with_cost(data, data_cost);

    let reset = new_test_frame_with_header(
        path,
        0,
        Header::QueueReset {
            queue_pair: crate::packet::datagram::QueuePair {
                source_queue_id: VarInt::from_u8(1),
                dest_queue_id: VarInt::from_u8(1),
            },
            binding_id: VarInt::from_u8(0),
            reset_target: crate::packet::datagram::ResetTarget::Both,
            error_code: VarInt::from_u8(7),
            init: None,
        },
    );
    let reset_cost = reset.byte_cost();
    batch.push_with_cost(reset, reset_cost);

    assert_eq!(
        batch.byte_cost(),
        MAX_FRAME_BATCH_PACKET_OVERHEAD + first_cost + data_cost + reset_cost
    );

    let (queues, costs) = batch.into_queues();
    assert_eq!(
        costs[Priority::QueueControl.as_index()],
        MAX_FRAME_BATCH_PACKET_OVERHEAD + first_cost
    );
    assert_eq!(costs[Priority::QueueData.as_index()], data_cost);
    assert_eq!(costs[Priority::QueueReset.as_index()], reset_cost);
    assert_eq!(queues[Priority::QueueControl.as_index()].len(), 1);
    assert_eq!(queues[Priority::QueueData.as_index()].len(), 1);
    assert_eq!(queues[Priority::QueueReset.as_index()].len(), 1);
}

#[test]
fn batch_frames_groups_by_same_path_secret() {
    let path_a = test_path_secret_entry();
    let path_b = test_path_secret_entry();
    path_a.update_max_datagram_size(4_096);
    path_b.update_max_datagram_size(4_096);

    let rx = TestReceiver {
        values: VecDeque::from([
            new_test_frame(path_a.clone(), 16),
            new_test_frame(path_a.clone(), 16),
            new_test_frame(path_b.clone(), 16),
        ]),
        consumed: 0,
    };
    let mut batcher = BatchFramesByPathSecret::new(rx, &test_clock(), Rate::new(10.0));

    let first = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    let Poll::Ready(Some(first)) = first else {
        panic!("expected first batch");
    };
    assert_eq!(first.len(), 2);
    assert!(Arc::ptr_eq(first.path_secret_entry(), &path_a));

    let second = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    let Poll::Ready(Some(second)) = second else {
        panic!("expected second batch");
    };
    assert_eq!(second.len(), 1);
    assert!(Arc::ptr_eq(second.path_secret_entry(), &path_b));
}

#[test]
fn batch_frames_enforces_datagram_byte_budget() {
    let path = test_path_secret_entry();
    // target_bytes = u16::MAX - 3000 ≈ 62535. Use frames large enough to exceed it.
    let frame_size = 40_000;

    let rx = TestReceiver {
        values: VecDeque::from([
            new_test_frame(path.clone(), frame_size),
            new_test_frame(path.clone(), frame_size),
            new_test_frame(path.clone(), frame_size),
        ]),
        consumed: 0,
    };
    let mut batcher = BatchFramesByPathSecret::new(rx, &test_clock(), Rate::new(10.0));

    let target_bytes = u16::MAX as u64 - 3000;

    let first = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    let Poll::Ready(Some(first)) = first else {
        panic!("expected first batch");
    };
    // First frame + overhead exceeds target, so only one frame per batch.
    assert_eq!(first.len(), 1);
    assert!(first.byte_cost() <= target_bytes);

    let second = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    let Poll::Ready(Some(second)) = second else {
        panic!("expected second batch");
    };
    assert_eq!(second.len(), 1);

    let third = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    let Poll::Ready(Some(third)) = third else {
        panic!("expected third batch");
    };
    assert_eq!(third.len(), 1);
}

#[test]
fn batch_frames_forwards_on_consumed() {
    let path = test_path_secret_entry();
    let rx = TestReceiver {
        values: VecDeque::from([new_test_frame(path, 0)]),
        consumed: 0,
    };
    let mut batcher = BatchFramesByPathSecret::new(rx, &test_clock(), Rate::new(10.0));

    batcher.on_consumed(321);
    assert_eq!(batcher.inner.consumed, 321);
}

/// [`test_clock`] that reports a timer resolution, as tokio's clock reports its 1 ms tick.
struct CoarseClock {
    clock: test_clock_mod::Clock,
    resolution: core::time::Duration,
}

impl s2n_quic_core::time::Clock for CoarseClock {
    fn get_time(&self) -> s2n_quic_core::time::Timestamp {
        s2n_quic_core::time::Clock::get_time(&self.clock)
    }
}

impl precision::Clock for CoarseClock {
    type Timer = test_clock_mod::Timer;

    fn now(&self) -> precision::Timestamp {
        precision::Clock::now(&self.clock)
    }

    fn timer(&self) -> Self::Timer {
        precision::Clock::timer(&self.clock)
    }

    fn resolution(&self) -> core::time::Duration {
        self.resolution
    }
}

/// A lone frame on a still-open channel, the shape of one small call: a clock whose timers
/// can't keep the batch wait (~50 us at 10 Gbps) emits its batch on the first poll and arms no
/// timer; a zero-resolution clock (busy-poll's) still holds it for the wait.
#[test]
fn batch_frames_lone_call_floor() {
    use crate::time::precision::Timer as _;

    let rate = Rate::new(10.0);
    let wait = core::time::Duration::from_nanos(rate.nanos_for_bytes(u16::MAX as u64 - 3000));

    let coarse = CoarseClock {
        clock: test_clock(),
        resolution: core::time::Duration::from_millis(1),
    };
    let (mut coarse_tx, rx) = unsync::new::<Frame>();
    let frame = new_test_frame(test_path_secret_entry(), 16);
    assert!(UnboundedSender::send(&mut coarse_tx, frame).is_ok());
    let mut batcher = BatchFramesByPathSecret::new(rx, &coarse, rate);
    let first = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    let Poll::Ready(Some(batch)) = first else {
        panic!("a lone frame's batch should go out on its first poll");
    };
    assert_eq!(batch.len(), 1);
    assert!(!batcher.timer.is_armed());

    let fine = test_clock();
    let (mut fine_tx, rx) = unsync::new::<Frame>();
    let frame = new_test_frame(test_path_secret_entry(), 16);
    assert!(UnboundedSender::send(&mut fine_tx, frame).is_ok());
    let mut batcher = BatchFramesByPathSecret::new(rx, &fine, rate);
    let first = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    assert!(first.is_pending());
    assert!(batcher.timer.is_armed());
    fine.advance(wait);
    let second = with_noop_context(|cx| batcher.poll_recv(cx, &mut Budget::new(usize::MAX)));
    let Poll::Ready(Some(batch)) = second else {
        panic!("the batch should go out once its wait has passed");
    };
    assert_eq!(batch.len(), 1);
}

#[test]
fn ack_processor_drops_message_with_out_of_range_sender_idx() {
    const OUT_OF_RANGE_SENDER_ID: u64 = 42; // total_sender_ids is 1, so any value > 0 is invalid.

    let registry = crate::counter::Registry::default();
    let send_caches: crate::endpoint::id::IdMap<crate::endpoint::id::LocalSendSocketId, _> =
        vec![Rc::new(RefCell::new(send::Cache::new(
            &registry,
            crate::endpoint::id::LocalSenderId::from_index(0),
        )))]
        .into();
    let sender_idx_to_local = crate::endpoint::id::IdMap::<
        crate::endpoint::id::LocalSenderId,
        crate::endpoint::id::LocalSendSocketId,
    >::new(1, crate::endpoint::id::LocalSendSocketId::new(0));
    let (frame_tx, _frame_rx) = frame::submission_channel(1);
    let (tx_wheel_tx, _tx_wheel_rx) = unsync::new_with_adapter::<send::TxWheelAdapter>();
    let (pto_wheel_tx, _pto_wheel_rx) = unsync::new_with_adapter::<send::PtoWheelAdapter>();
    let (idle_wheel_tx, _idle_wheel_rx) = unsync::new_with_adapter::<send::IdleWheelAdapter>();
    let path_secret_entry = test_path_secret_entry();

    let ack_rx = TestReceiver {
        values: VecDeque::from([Entry::new(msg::Sender::ReceivedAck {
            local_sender_id: crate::endpoint::id::LocalSenderId::new(
                VarInt::new(OUT_OF_RANGE_SENDER_ID).expect("valid varint"),
            ),
            path_secret_entry,
            payload: BytesMut::new(),
            ack_delay: core::time::Duration::ZERO,
            largest_acknowledged: VarInt::ZERO,
            ack_range: VarInt::ZERO,
            ecn_counts: Default::default(),
        })]),
        consumed: 0,
    };

    let (ack_completions_tx, _ack_completions_rx) = unsync::new::<msg::Sender>();
    let ack_completions_tx = ack_completions_tx.into_list_sender();
    let processor = AckProcessor::new(
        ack_rx,
        send_caches,
        sender_idx_to_local,
        1,
        crate::time::bach::Clock::default(),
        crate::xorshift::Rng::new(),
        frame_tx,
        frame::PriorityInput::default(),
        frame::PriorityInput::default(),
        ack_completions_tx,
        registry.register("!send.invalid_sender_idx"),
    );
    let rx = crate::socket::channel::Flatten::new(processor);
    let (immediate_tx, _immediate_rx) = unsync::new_with_adapter::<send::TxImmediateAdapter>();
    let mut router = crate::stream::endpoint::send::WheelRouter::new(
        rx,
        immediate_tx,
        tx_wheel_tx,
        pto_wheel_tx,
        idle_wheel_tx,
    );

    // The invalid sender_idx message is consumed (Flatten skips the None),
    // then the input is exhausted so the channel closes.
    let result = with_noop_context(|cx| router.poll_recv(cx, &mut Budget::new(usize::MAX)));
    assert_eq!(result, Poll::Ready(None));
}
