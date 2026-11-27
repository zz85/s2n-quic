// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use crate::tracing::*;
use core::time::Duration;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use s2n_quic_dc_metrics::Summary as RawSummary;
pub use s2n_quic_dc_metrics::Unit;

/// Builds the emitted aggregation for a nominal metric's variant.
///
/// Every nominal dimension is keyed as `Variant|<value>` — the single convention the querylog
/// consumer expects, applied uniformly to semantic variants (`Variant|ect0`) and per-worker
/// breakdowns (`Variant|send.5`) alike. Keeping one key means a downstream backend policy collapses
/// a metric by targeting the `Variant` key, scoped by name/tag matchers; the querylog line carries a
/// single aggregation per metric (it has no way to encode more than one). The dc-side
/// [`MetricMetadata`] keeps the raw value for topology/trace display.
fn variant_aggregation(variant: &str) -> String {
    format!("Variant|{variant}")
}

/// Stable identifier for metric metadata tracked in [`Registry`].
pub type MetricId = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetricKind {
    Counter,
    Gauge,
    Summary,
    Timer,
}

impl core::fmt::Display for MetricKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Counter => f.write_str("counter"),
            Self::Gauge => f.write_str("gauge"),
            Self::Summary => f.write_str("summary"),
            Self::Timer => f.write_str("timer"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MetricMetadata {
    pub id: MetricId,
    pub label: String,
    pub variant: Option<String>,
    pub kind: MetricKind,
    pub unit: Option<&'static str>,
    pub description: String,
}

/// Emits a compact metric trace event.
///
/// Format: `{prefix}#{label}={value}` or `{prefix}#{label}[{variant}]={value}`.
/// The `target` is set to `"s2n_quic_dc::metric"` so the snapshot subscriber's
/// env-filter directive (`s2n_quic_dc::metric=trace`) can enable it, while the
/// snapshot formatter suppresses the target string itself with `with_target(false)`.
#[cfg(any(test, feature = "metric-tracing"))]
macro_rules! metric_emit {
    ($prefix:literal, $label:expr, $variant:expr, $value:expr) => {
        match $variant.as_deref() {
            Some(v) => trace!(
                target: "s2n_quic_dc::metric",
                concat!($prefix, "#{}[{}]={}"),
                $label,
                v,
                $value
            ),
            None => trace!(
                target: "s2n_quic_dc::metric",
                concat!($prefix, "#{}={}"),
                $label,
                $value
            ),
        }
    };
}

/// Returns the `(label, variant)` for a metric from the shared metadata map.
#[cfg(any(test, feature = "metric-tracing"))]
#[inline]
fn metric_trace_fields(
    metric_id: MetricId,
    metadata: &Arc<Mutex<HashMap<MetricId, MetricMetadata>>>,
) -> (String, Option<String>) {
    let metadata = metadata.lock().unwrap().get(&metric_id).cloned();
    match metadata {
        Some(metadata) => (metadata.label, metadata.variant),
        None => ("unknown".to_string(), None),
    }
}

/// Returns `true` when the current thread is running inside a bach discrete-event
/// simulation.  In that context wall-clock [`Instant`] values are synthetic and
/// non-deterministic timer durations should not be emitted to snapshots.
#[cfg(any(test, feature = "metric-tracing"))]
#[inline]
fn in_bach_sim() -> bool {
    #[cfg(any(test, feature = "testing"))]
    {
        bach::is_active()
    }

    #[cfg(not(any(test, feature = "testing")))]
    {
        false
    }
}

/// Spawns a background thread that periodically **drains** the metric registry's page pool without
/// exporting anything.
///
/// Recording accumulates into a per-CPU/full-page pool that only shrinks when something drives a
/// report; with nothing draining it, the pool grows without bound (a real ~GB/min leak under load).
/// A process that records dc metrics but installs no exporting reporter of its own (tests, ad-hoc
/// binaries) can call this to keep the pool bounded. The drain is destructive but discards the
/// values (reports into the no-op `()` backend), so it does not interfere with — and should not be
/// run alongside — an exporting reporter over the same registry.
///
/// The loop exits when the registry is [closed](s2n_quic_dc_metrics::Registry::close). Errors
/// spawning the thread are logged and otherwise ignored (the pool simply won't be drained).
pub fn spawn_noop_drain_reporter(registry: &Registry, interval: Duration) {
    let inner = registry.inner.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("s2n-quic-dc-drain".into())
        .spawn(move || loop {
            std::thread::sleep(interval);
            if !inner.is_open() {
                break;
            }
            // Destructive drain into the no-op backend: folds per-CPU pages back into the aggregate
            // (bounding the pool) and discards the reported values.
            inner.report(&mut ());
        })
    {
        warn!(%error, "failed to spawn s2n-quic-dc drain reporter thread");
    }
}

// ── Counter ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Counter {
    inner: s2n_quic_dc_metrics::Counter,
    metric_id: MetricId,
    metadata: Arc<Mutex<HashMap<MetricId, MetricMetadata>>>,
}

impl Counter {
    #[inline]
    pub fn add(&self, v: u64) {
        #[cfg(any(test, feature = "metric-tracing"))]
        {
            let (label, variant) = metric_trace_fields(self.metric_id, &self.metadata);
            metric_emit!("c", label, variant, v);
        }
        self.inner.increment(v);
    }

    #[inline]
    pub fn metric_metadata(&self) -> Option<MetricMetadata> {
        self.metadata.lock().unwrap().get(&self.metric_id).cloned()
    }

    #[inline]
    pub fn with_description(self, description: impl core::fmt::Display) -> Self {
        if let Some(metadata) = self.metadata.lock().unwrap().get_mut(&self.metric_id) {
            metadata.description = description.to_string();
        }
        self
    }
}

impl core::ops::AddAssign<u64> for Counter {
    #[inline]
    fn add_assign(&mut self, rhs: u64) {
        self.add(rhs);
    }
}

// ── Gauge ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Gauge {
    inner: Arc<AtomicI64>,
    metric_id: MetricId,
    metadata: Arc<Mutex<HashMap<MetricId, MetricMetadata>>>,
}

impl Gauge {
    #[inline]
    pub fn add(&self, v: i64) -> i64 {
        let value = self.inner.fetch_add(v, Ordering::Relaxed) + v;
        #[cfg(any(test, feature = "metric-tracing"))]
        {
            let (label, variant) = metric_trace_fields(self.metric_id, &self.metadata);
            metric_emit!("g", label, variant, value);
        }
        value
    }

    #[inline]
    pub fn sub(&self, v: i64) -> i64 {
        let value = self.inner.fetch_sub(v, Ordering::Relaxed) - v;
        #[cfg(any(test, feature = "metric-tracing"))]
        {
            let (label, variant) = metric_trace_fields(self.metric_id, &self.metadata);
            metric_emit!("g", label, variant, value);
        }
        value
    }

    #[inline]
    pub fn get(&self) -> i64 {
        self.inner.load(Ordering::Relaxed)
    }

    /// Sets the gauge to an absolute value.
    #[inline]
    pub fn set(&self, v: i64) {
        self.inner.store(v, Ordering::Relaxed);
    }

    #[inline]
    pub fn metric_metadata(&self) -> Option<MetricMetadata> {
        self.metadata.lock().unwrap().get(&self.metric_id).cloned()
    }

    #[inline]
    pub fn with_description(self, description: impl core::fmt::Display) -> Self {
        if let Some(metadata) = self.metadata.lock().unwrap().get_mut(&self.metric_id) {
            metadata.description = description.to_string();
        }
        self
    }
}

// ── Timer ───────────────────────────────────────────────────────────────────

/// Single-threaded counter that is `Send + Sync` for crossing thread boundaries at setup time.
///
/// Once a `Timer` is moved onto its owning executor thread, the `Sampler` is only ever
/// accessed from that one thread. We enforce this structurally: `Timer` is cloned during
/// setup, then each clone is moved into exactly one task closure — no concurrent access.
struct Sampler(std::cell::UnsafeCell<u8>);

impl Clone for Sampler {
    fn clone(&self) -> Self {
        Self::new()
    }
}

// SAFETY: Sampler is only mutated from a single thread after setup. Cloning produces
// an independent instance per task — no shared mutable state.
unsafe impl Send for Sampler {}
unsafe impl Sync for Sampler {}

impl Sampler {
    fn new() -> Self {
        Self(std::cell::UnsafeCell::new(0))
    }

    #[inline]
    fn next(&self) -> u8 {
        // SAFETY: only called from the owning executor thread
        unsafe {
            let ptr = self.0.get();
            let c = (*ptr).wrapping_add(1);
            *ptr = c;
            c
        }
    }
}

/// A histogram that records elapsed durations via a guard pattern.
///
/// Call `timer.start()` to begin timing; the returned guard records
/// the elapsed duration into the underlying `Summary` on drop.
///
/// By default, timing is sampled at 1-in-64 to minimize overhead on hot paths.
/// Use [`Timer::unsampled`] to get a timer that records every call.
#[derive(Clone)]
pub struct Timer {
    summary: RawSummary,
    metric_id: MetricId,
    metadata: Arc<Mutex<HashMap<MetricId, MetricMetadata>>>,
    sampler: Sampler,
    sample_mask: u8,
}

impl Timer {
    /// Sample 1-in-64: only record timing on every 64th call.
    const DEFAULT_SAMPLE_MASK: u8 = 63;

    /// Returns a version of this timer that records every call (no sampling).
    #[inline]
    pub fn unsampled(mut self) -> Self {
        self.sample_mask = 0;
        self
    }

    /// Returns true if this call should be sampled (and advances the counter).
    ///
    /// Under bach simulations, always returns false — CPU wall-clock timings
    /// are nondeterministic and would destabilize snapshot tests.
    #[inline]
    fn should_sample(&self) -> bool {
        #[cfg(any(test, feature = "testing"))]
        if bach::is_active() {
            return false;
        }
        self.sampler.next() & self.sample_mask == 0
    }

    #[inline]
    pub fn start(&self) -> Option<TimerGuard<'_>> {
        if !self.should_sample() {
            return None;
        }
        Some(self.start_unchecked())
    }

    /// Starts a timer unconditionally (bypasses sampling).
    #[inline]
    fn start_unchecked(&self) -> TimerGuard<'_> {
        self.start_at(Instant::now())
    }

    /// Starts a timer from a caller-provided `Instant`.
    #[inline]
    pub fn start_at(&self, start: Instant) -> TimerGuard<'_> {
        #[cfg(any(test, feature = "metric-tracing"))]
        let (metric_label, metric_variant) = metric_trace_fields(self.metric_id, &self.metadata);
        #[cfg(not(any(test, feature = "metric-tracing")))]
        let (metric_label, metric_variant) = (String::new(), None);
        TimerGuard {
            summary: &self.summary,
            metric_label,
            metric_variant,
            start,
            recorded: false,
        }
    }

    #[inline]
    pub fn record(&self, duration: Duration) {
        if !self.should_sample() {
            return;
        }
        #[cfg(any(test, feature = "metric-tracing"))]
        if !in_bach_sim() {
            let (label, variant) = metric_trace_fields(self.metric_id, &self.metadata);
            metric_emit!("t", label, variant, duration.as_micros());
        }
        self.summary.record_duration(duration);
    }

    #[inline]
    pub fn metric_metadata(&self) -> Option<MetricMetadata> {
        self.metadata.lock().unwrap().get(&self.metric_id).cloned()
    }

    #[inline]
    pub fn with_description(self, description: impl core::fmt::Display) -> Self {
        if let Some(metadata) = self.metadata.lock().unwrap().get_mut(&self.metric_id) {
            metadata.description = description.to_string();
        }
        self
    }
}

#[derive(Clone)]
pub struct Summary {
    summary: RawSummary,
    metric_id: MetricId,
    metadata: Arc<Mutex<HashMap<MetricId, MetricMetadata>>>,
}

impl Summary {
    #[inline]
    pub fn record_value(&self, value: u64) {
        #[cfg(any(test, feature = "metric-tracing"))]
        {
            let (label, variant) = metric_trace_fields(self.metric_id, &self.metadata);
            metric_emit!("m", label, variant, value);
        }
        self.summary.record_value(value);
    }

    #[inline]
    pub fn metric_metadata(&self) -> Option<MetricMetadata> {
        self.metadata.lock().unwrap().get(&self.metric_id).cloned()
    }

    #[inline]
    pub fn with_description(self, description: impl core::fmt::Display) -> Self {
        if let Some(metadata) = self.metadata.lock().unwrap().get_mut(&self.metric_id) {
            metadata.description = description.to_string();
        }
        self
    }
}

pub struct TimerGuard<'a> {
    summary: &'a RawSummary,
    #[allow(dead_code)]
    metric_label: String,
    #[allow(dead_code)]
    metric_variant: Option<String>,
    start: Instant,
    recorded: bool,
}

impl TimerGuard<'_> {
    #[inline]
    pub fn record(mut self) -> Instant {
        let now = Instant::now();
        let duration = now.duration_since(self.start);
        #[cfg(any(test, feature = "metric-tracing"))]
        if !in_bach_sim() {
            metric_emit!(
                "t",
                self.metric_label,
                self.metric_variant,
                duration.as_micros()
            );
        }
        self.summary.record_duration(duration);
        self.recorded = true;
        now
    }
}

impl Drop for TimerGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        if !self.recorded {
            let duration = self.start.elapsed();
            #[cfg(any(test, feature = "metric-tracing"))]
            if !in_bach_sim() {
                metric_emit!(
                    "t",
                    self.metric_label,
                    self.metric_variant,
                    duration.as_micros()
                );
            }
            self.summary.record_duration(duration);
        }
    }
}

// ── Task ────────────────────────────────────────────────────────────────────

/// Metric bundle for a drain-budgeted task poll loop.
///
/// - `drained`: number of items processed in a single poll
/// - `time`: wall-clock duration spent inside a poll
/// - `next_poll_latency`: elapsed time from the end of one poll to the start
///   of the next poll
#[derive(Clone)]
pub struct Task {
    registry: Registry,
    registration: Arc<Mutex<TaskRegistrationMetadata>>,
    pub drained: Summary,
    pub time: Timer,
    pub next_poll_latency: Timer,
}

#[derive(Clone, Default)]
struct TaskRegistrationMetadata {
    name: String,
    description: String,
    function: String,
    budget: Option<usize>,
    worker_id: Option<usize>,
}

impl Task {
    pub const DRAINED_DESCRIPTION: &'static str =
        "Number of items processed per poll for this task";
    pub const DRAINED_UNIT: &'static str = "count";
    pub const TIME_DESCRIPTION: &'static str = "Wall-clock duration spent inside each task poll";
    pub const TIME_UNIT: &'static str = "microsecond";
    pub const NEXT_POLL_LATENCY_DESCRIPTION: &'static str =
        "Wall-clock latency between consecutive task polls";
    pub const NEXT_POLL_LATENCY_UNIT: &'static str = "microsecond";

    pub fn metrics(&self) -> Vec<MetricMetadata> {
        [
            self.drained.metric_metadata(),
            self.time.metric_metadata(),
            self.next_poll_latency.metric_metadata(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub fn with_registration_metadata_ref<T>(
        &self,
        f: impl FnOnce(&str, &str, &str, Option<usize>) -> T,
    ) -> T {
        let registration = self.registration.lock().unwrap();
        f(
            registration.name.as_str(),
            registration.description.as_str(),
            registration.function.as_str(),
            registration.budget,
        )
    }

    pub fn with_registration_metadata(
        self,
        name: impl core::fmt::Display,
        description: impl core::fmt::Display,
        function: impl core::fmt::Display,
    ) -> Self {
        self.with_registration_name(name)
            .with_registration_description(description)
            .with_registration_function(function)
    }

    pub fn with_registration_name(self, name: impl core::fmt::Display) -> Self {
        {
            let mut registration = self.registration.lock().unwrap();
            registration.name = name.to_string();
        }
        self
    }

    pub fn with_registration_description(self, description: impl core::fmt::Display) -> Self {
        {
            let mut registration = self.registration.lock().unwrap();
            registration.description = description.to_string();
        }
        self
    }

    pub fn with_registration_function(self, function: impl core::fmt::Display) -> Self {
        {
            let mut registration = self.registration.lock().unwrap();
            registration.function = function.to_string();
        }
        self
    }

    /// Called when the task is spawned to record spawn-time information in the topology.
    ///
    /// This captures the budget and worker ID, then registers the task in the topology.
    /// Panics in debug builds if the task has already been spawned.
    pub fn on_spawn(&self, budget: Option<usize>, worker_id: usize) {
        let mut registration = self.registration.lock().unwrap();
        debug_assert!(
            registration.worker_id.is_none(),
            "Task '{}' has already been spawned on worker {:?}",
            registration.name,
            registration.worker_id
        );
        registration.budget = budget;
        registration.worker_id = Some(worker_id);

        // Register in topology now that spawn information is finalized
        let task_registration = TaskRegistration::new(
            &registration.name,
            &registration.description,
            &registration.function,
        )
        .with_budget(budget)
        .with_worker_id(Some(worker_id))
        .with_metric(self);

        drop(registration);
        self.registry.register_task_topology(task_registration);
    }
}

// ── QueueGauge ──────────────────────────────────────────────────────────────
// TODO: add per-item sojourn time tracking (enqueue→dequeue latency as a histogram)

#[derive(Clone)]
pub struct QueueGauge {
    registry: Registry,
    key: String,
    pub throughput: Counter,
    pub drain: Counter,
    pub depth: Gauge,
    pub depth_distribution: Summary,
}

/// A [`QueueGauge`]'s metric handles, as [`Registry`] caches them by queue name. The cache leaves
/// out the gauge's [`Registry`] handle: that handle holds the cache, so a cached gauge would keep
/// its registry, and every metric registered through it, alive after the last other handle drops.
#[derive(Clone)]
struct CachedQueueGauge {
    key: String,
    throughput: Counter,
    drain: Counter,
    depth: Gauge,
    depth_distribution: Summary,
}

impl CachedQueueGauge {
    fn new(gauge: &QueueGauge) -> Self {
        Self {
            key: gauge.key.clone(),
            throughput: gauge.throughput.clone(),
            drain: gauge.drain.clone(),
            depth: gauge.depth.clone(),
            depth_distribution: gauge.depth_distribution.clone(),
        }
    }

    fn gauge(&self, registry: &Registry) -> QueueGauge {
        QueueGauge {
            registry: registry.clone(),
            key: self.key.clone(),
            throughput: self.throughput.clone(),
            drain: self.drain.clone(),
            depth: self.depth.clone(),
            depth_distribution: self.depth_distribution.clone(),
        }
    }
}

#[derive(Clone, Default)]
struct QueueRegistrationMetadata {
    label: String,
    variant: Option<String>,
    name: String,
    description: String,
    function: String,
}

#[derive(Clone, Default)]
struct QueueEndpointMetadata {
    task_name: String,
    function: String,
}

#[derive(Clone)]
pub struct QueueSender {
    queue: QueueGauge,
    metric: Counter,
}

#[derive(Clone)]
pub struct QueueReceiver {
    queue: QueueGauge,
    metric: Counter,
}

impl QueueGauge {
    pub const ENQUEUED_DESCRIPTION: &'static str = "Total queue enqueue events";
    pub const ENQUEUED_UNIT: &'static str = "count";
    pub const DEQUEUED_DESCRIPTION: &'static str = "Total queue dequeue events";
    pub const DEQUEUED_UNIT: &'static str = "count";
    pub const DEPTH_DESCRIPTION: &'static str = "Current queue depth";
    pub const DEPTH_UNIT: &'static str = "count";
    pub const DEPTH_DISTRIBUTION_DESCRIPTION: &'static str =
        "Distribution of observed queue depth values";
    pub const DEPTH_DISTRIBUTION_UNIT: &'static str = "count";

    pub fn metrics(&self) -> Vec<MetricMetadata> {
        [
            self.throughput.metric_metadata(),
            self.drain.metric_metadata(),
            self.depth.metric_metadata(),
            self.depth_distribution.metric_metadata(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub fn with_registration_metadata(
        self,
        name: impl core::fmt::Display,
        description: impl core::fmt::Display,
        function: impl core::fmt::Display,
    ) -> Self {
        let queue = self
            .with_registration_name(name)
            .with_registration_description(description)
            .with_registration_function(function);

        // Update topology with new metadata
        let registration = queue.with_registration_metadata_ref(|name, description, function| {
            ChannelRegistration::new(name, description, function).with_metric(&queue)
        });
        queue.registry.upsert_channel_topology(registration);

        queue
    }

    pub fn with_registration_name(self, name: impl core::fmt::Display) -> Self {
        if let Some(registration) = self
            .registry
            .queue_metadata
            .lock()
            .unwrap()
            .get_mut(&self.key)
        {
            registration.name = name.to_string();
        }
        self
    }

    pub fn with_registration_description(self, description: impl core::fmt::Display) -> Self {
        if let Some(registration) = self
            .registry
            .queue_metadata
            .lock()
            .unwrap()
            .get_mut(&self.key)
        {
            registration.description = description.to_string();
        }
        self
    }

    pub fn with_registration_function(self, function: impl core::fmt::Display) -> Self {
        if let Some(registration) = self
            .registry
            .queue_metadata
            .lock()
            .unwrap()
            .get_mut(&self.key)
        {
            registration.function = function.to_string();
        }
        self
    }

    pub fn with_registration_metadata_ref<T>(&self, f: impl FnOnce(&str, &str, &str) -> T) -> T {
        let registrations = self.registry.queue_metadata.lock().unwrap();
        let registration = registrations
            .get(&self.key)
            .expect("queue registration metadata missing");
        f(
            registration.name.as_str(),
            registration.description.as_str(),
            registration.function.as_str(),
        )
    }

    fn register_endpoint_counter(
        &self,
        suffix: &str,
        task_name: &str,
        description: impl core::fmt::Display,
    ) -> Counter {
        let registrations = self.registry.queue_metadata.lock().unwrap();
        let registration = registrations
            .get(&self.key)
            .expect("queue registration metadata missing");
        let label = format!("{}.{}", registration.label, suffix);
        // A metric carries a single aggregation dimension. The endpoint counter takes the queue's
        // own variant (e.g. the worker index), so it collapses together with the rest of that
        // queue's metrics (`.enq`/`.drain`/`.depth`) under a downstream `collapse Variant`. The
        // endpoint (sender vs receiver, and which task) is already encoded by the `.sender`/
        // `.receiver` name suffix and kept in the endpoint metadata for display. A queue with no
        // variant falls back to the task name as its dimension.
        let (variant, agg) = match registration.variant.as_deref() {
            Some(queue_variant) => (
                queue_variant.to_string(),
                Some(variant_aggregation(queue_variant)),
            ),
            None => (task_name.to_string(), Some(variant_aggregation(task_name))),
        };
        drop(registrations);
        let metric_id = self.registry.register_metric_metadata(
            &label,
            Some(&variant),
            MetricKind::Counter,
            Some("count"),
            description,
        );

        self.registry
            .counter_handle(self.registry.inner.register_counter(label, agg), metric_id)
    }

    pub fn sender(&self, task_name: impl core::fmt::Display) -> QueueSender {
        let task_name = task_name.to_string();
        let metric = self.register_endpoint_counter(
            "sender",
            &task_name,
            format_args!("Queue sends performed by {task_name}"),
        );
        self.registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .insert(
                metric.metric_id,
                QueueEndpointMetadata {
                    task_name,
                    ..Default::default()
                },
            );
        let sender = QueueSender {
            queue: self.clone(),
            metric,
        };

        // Auto-register in topology
        let channel_name = sender.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            sender.task_name(),
            channel_name,
            ChannelDirection::Sends,
            sender.description(),
            sender.function(),
        );
        self.registry.register_binding_topology(binding);

        sender
    }

    pub fn receiver(&self, task_name: impl core::fmt::Display) -> QueueReceiver {
        let task_name = task_name.to_string();
        let metric = self.register_endpoint_counter(
            "receiver",
            &task_name,
            format_args!("Queue receives performed by {task_name}"),
        );
        self.registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .insert(
                metric.metric_id,
                QueueEndpointMetadata {
                    task_name,
                    ..Default::default()
                },
            );
        let receiver = QueueReceiver {
            queue: self.clone(),
            metric,
        };

        // Auto-register in topology
        let channel_name = receiver.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            receiver.task_name(),
            channel_name,
            ChannelDirection::Receives,
            receiver.description(),
            receiver.function(),
        );
        self.registry.register_binding_topology(binding);

        receiver
    }
}

impl QueueGauge {
    #[inline]
    pub fn enqueue(&self, count: u64) {
        self.throughput.add(count);
        let depth = self.depth.add(count as i64).max(0) as u64;
        self.depth_distribution.record_value(depth);
    }

    #[inline]
    pub fn dequeue(&self) {
        self.drain.add(1);
        let _ = self.depth.sub(1).max(0) as u64;
    }

    #[inline]
    pub fn dequeue_n(&self, count: u64) {
        self.drain.add(count);
        let _ = self.depth.sub(count as i64).max(0) as u64;
    }
}

impl QueueSender {
    pub fn with_description(mut self, description: impl core::fmt::Display) -> Self {
        self.metric = self.metric.with_description(description);

        // Update binding in topology
        let channel_name = self.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            self.task_name(),
            channel_name,
            ChannelDirection::Sends,
            self.description(),
            self.function(),
        );
        self.queue.registry.upsert_binding_topology(binding);

        self
    }

    pub fn with_function(self, function: impl core::fmt::Display) -> Self {
        if let Some(metadata) = self
            .queue
            .registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .get_mut(&self.metric.metric_id)
        {
            metadata.function = function.to_string();
        }

        // Update binding in topology
        let channel_name = self.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            self.task_name(),
            channel_name,
            ChannelDirection::Sends,
            self.description(),
            self.function(),
        );
        self.queue.registry.upsert_binding_topology(binding);

        self
    }

    pub(crate) fn task_name(&self) -> String {
        self.queue
            .registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .get(&self.metric.metric_id)
            .map(|metadata| metadata.task_name.clone())
            .unwrap_or_default()
    }

    pub fn channel_metadata<T>(&self, f: impl FnOnce(&str, &str, &str) -> T) -> T {
        self.queue.with_registration_metadata_ref(f)
    }

    pub fn metrics(&self) -> Vec<MetricMetadata> {
        self.metric.metric_metadata().into_iter().collect()
    }

    pub(crate) fn description(&self) -> String {
        self.metric
            .metric_metadata()
            .map(|metadata| metadata.description)
            .unwrap_or_default()
    }

    pub(crate) fn function(&self) -> String {
        self.queue
            .registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .get(&self.metric.metric_id)
            .map(|metadata| metadata.function.clone())
            .unwrap_or_default()
    }

    #[inline]
    fn on_send(&self, count: u64) {
        self.metric.add(count);
        self.queue.enqueue(count);
    }
}

impl QueueReceiver {
    pub fn with_description(mut self, description: impl core::fmt::Display) -> Self {
        self.metric = self.metric.with_description(description);

        // Update binding in topology
        let channel_name = self.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            self.task_name(),
            channel_name,
            ChannelDirection::Receives,
            self.description(),
            self.function(),
        );
        self.queue.registry.upsert_binding_topology(binding);

        self
    }

    pub fn with_function(self, function: impl core::fmt::Display) -> Self {
        if let Some(metadata) = self
            .queue
            .registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .get_mut(&self.metric.metric_id)
        {
            metadata.function = function.to_string();
        }

        // Update binding in topology
        let channel_name = self.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            self.task_name(),
            channel_name,
            ChannelDirection::Receives,
            self.description(),
            self.function(),
        );
        self.queue.registry.upsert_binding_topology(binding);

        self
    }

    pub(crate) fn task_name(&self) -> String {
        self.queue
            .registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .get(&self.metric.metric_id)
            .map(|metadata| metadata.task_name.clone())
            .unwrap_or_default()
    }

    pub fn channel_metadata<T>(&self, f: impl FnOnce(&str, &str, &str) -> T) -> T {
        self.queue.with_registration_metadata_ref(f)
    }

    pub fn metrics(&self) -> Vec<MetricMetadata> {
        self.metric.metric_metadata().into_iter().collect()
    }

    pub(crate) fn description(&self) -> String {
        self.metric
            .metric_metadata()
            .map(|metadata| metadata.description)
            .unwrap_or_default()
    }

    pub(crate) fn function(&self) -> String {
        self.queue
            .registry
            .queue_endpoint_metadata
            .lock()
            .unwrap()
            .get(&self.metric.metric_id)
            .map(|metadata| metadata.function.clone())
            .unwrap_or_default()
    }

    #[inline]
    fn on_receive(&self, count: u64) {
        self.metric.add(count);
        self.queue.dequeue_n(count);
    }
}

// ── Metric Registration Structures ──────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct MetricRegistration {
    pub label: String,
    pub variant: Option<String>,
    pub kind: MetricKind,
    pub unit: Option<&'static str>,
    pub description: String,
}

impl MetricRegistration {
    #[inline]
    pub fn new(
        name: impl core::fmt::Display,
        kind: MetricKind,
        description: impl core::fmt::Display,
    ) -> Self {
        Self {
            label: name.to_string(),
            variant: None,
            kind,
            unit: None,
            description: description.to_string(),
        }
    }

    #[inline]
    pub fn with_variant(mut self, variant: impl core::fmt::Display) -> Self {
        self.variant = Some(variant.to_string());
        self
    }

    #[inline]
    pub fn with_unit(mut self, unit: &'static str) -> Self {
        self.unit = Some(unit);
        self
    }
}

/// Controls how metrics are rendered in a Mermaid node.
///
/// Metrics are always embedded as structured `%% metric:` comments on each node
/// so a custom Mermaid extension can read them. The comments use the name returned
/// by [`format_metric_name`], so namespace prefixes and other transformations are
/// reflected there too.
///
/// [`should_display_metric`] separately controls whether the metric list is also
/// shown inline in the visible node card — useful for maintainer views vs.
/// end-user charts where only live values matter.
///
/// [`format_metric_name`]: MermaidMetricFormatter::format_metric_name
/// [`should_display_metric`]: MermaidMetricFormatter::should_display_metric
pub trait MermaidMetricFormatter {
    /// Return the display name for this metric (used in both the `%% metric:`
    /// comment and the visible card line).  Implement this to add a namespace
    /// prefix or map to a backend-specific identifier.
    fn format_metric_name(&self, metric: &MetricRegistration) -> String;

    /// Return `true` to include this metric in the visible node card, or `false`
    /// to hide it (it still appears in the `%% metric:` comments).
    fn should_display_metric(&self, metric: &MetricRegistration) -> bool;
}

/// Default [`MermaidMetricFormatter`] that renders each metric name as-is and
/// shows all metrics inline in the node card.
#[derive(Clone, Debug, Default)]
pub struct DefaultMermaidMetricFormatter;

impl MermaidMetricFormatter for DefaultMermaidMetricFormatter {
    fn format_metric_name(&self, metric: &MetricRegistration) -> String {
        metric.label.clone()
    }

    fn should_display_metric(&self, _metric: &MetricRegistration) -> bool {
        true
    }
}

pub trait Metric {
    fn registrations(&self) -> Vec<MetricRegistration>;
}

impl Metric for MetricRegistration {
    fn registrations(&self) -> Vec<MetricRegistration> {
        vec![self.clone()]
    }
}

impl Metric for Task {
    fn registrations(&self) -> Vec<MetricRegistration> {
        self.metrics()
            .into_iter()
            .map(|metric| {
                let mut registration =
                    MetricRegistration::new(metric.label, metric.kind, metric.description);
                if let Some(variant) = metric.variant {
                    registration = registration.with_variant(variant);
                }
                if let Some(unit) = metric.unit {
                    registration = registration.with_unit(unit);
                }
                registration
            })
            .collect()
    }
}

impl Metric for QueueGauge {
    fn registrations(&self) -> Vec<MetricRegistration> {
        self.metrics()
            .into_iter()
            .map(|metric| {
                let mut registration =
                    MetricRegistration::new(metric.label, metric.kind, metric.description);
                if let Some(variant) = metric.variant {
                    registration = registration.with_variant(variant);
                }
                if let Some(unit) = metric.unit {
                    registration = registration.with_unit(unit);
                }
                registration
            })
            .collect()
    }
}

impl Metric for QueueSender {
    fn registrations(&self) -> Vec<MetricRegistration> {
        self.metric
            .metric_metadata()
            .into_iter()
            .map(|metadata| MetricRegistration {
                label: metadata.label,
                variant: metadata.variant,
                kind: metadata.kind,
                unit: metadata.unit,
                description: metadata.description,
            })
            .collect()
    }
}

impl Metric for QueueReceiver {
    fn registrations(&self) -> Vec<MetricRegistration> {
        self.metric
            .metric_metadata()
            .into_iter()
            .map(|metadata| MetricRegistration {
                label: metadata.label,
                variant: metadata.variant,
                kind: metadata.kind,
                unit: metadata.unit,
                description: metadata.description,
            })
            .collect()
    }
}

/// Describes a spawned pipeline task for runtime-level introspection.
#[derive(Clone, Debug)]
pub struct TaskRegistration {
    pub name: String,
    pub description: String,
    pub function: String,
    pub budget: Option<usize>,
    pub worker_id: Option<usize>,
    pub metrics: Vec<MetricRegistration>,
}

impl TaskRegistration {
    #[inline]
    pub fn new(
        name: impl core::fmt::Display,
        description: impl core::fmt::Display,
        function: impl core::fmt::Display,
    ) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            function: function.to_string(),
            budget: None,
            worker_id: None,
            metrics: Vec::new(),
        }
    }

    #[inline]
    pub fn with_budget(mut self, budget: Option<usize>) -> Self {
        self.budget = budget;
        self
    }

    #[inline]
    pub fn with_worker_id(mut self, worker_id: Option<usize>) -> Self {
        self.worker_id = worker_id;
        self
    }

    #[inline]
    pub fn with_metric(mut self, metric: &impl Metric) -> Self {
        for metric in metric.registrations() {
            self.upsert_metric(metric);
        }
        self
    }

    #[inline]
    fn upsert_metric(&mut self, metric: MetricRegistration) {
        if let Some(current) = self
            .metrics
            .iter_mut()
            .find(|m| m.label == metric.label && m.variant == metric.variant)
        {
            *current = metric;
        } else {
            self.metrics.push(metric);
        }
    }
}

/// Describes a channel/queue entity in the runtime pipeline.
#[derive(Clone, Debug)]
pub struct ChannelRegistration {
    pub name: String,
    pub description: String,
    pub function: String,
    pub metrics: Vec<MetricRegistration>,
}

impl ChannelRegistration {
    #[inline]
    pub fn new(
        name: impl core::fmt::Display,
        description: impl core::fmt::Display,
        function: impl core::fmt::Display,
    ) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            function: function.to_string(),
            metrics: Vec::new(),
        }
    }

    #[inline]
    pub fn with_metric(mut self, metric: &impl Metric) -> Self {
        for metric in metric.registrations() {
            self.upsert_metric(metric);
        }
        self
    }

    #[inline]
    fn upsert_metric(&mut self, metric: MetricRegistration) {
        if let Some(current) = self
            .metrics
            .iter_mut()
            .find(|m| m.label == metric.label && m.variant == metric.variant)
        {
            *current = metric;
        } else {
            self.metrics.push(metric);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelDirection {
    Sends,
    Receives,
}

impl core::fmt::Display for ChannelDirection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Sends => f.write_str("sends"),
            Self::Receives => f.write_str("receives"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChannelBinding {
    pub task_name: String,
    pub channel_name: String,
    pub direction: ChannelDirection,
    pub description: String,
    pub function: String,
}

impl ChannelBinding {
    #[inline]
    pub fn new(
        task_name: impl core::fmt::Display,
        channel_name: impl core::fmt::Display,
        direction: ChannelDirection,
        description: impl core::fmt::Display,
        function: impl core::fmt::Display,
    ) -> Self {
        Self {
            task_name: task_name.to_string(),
            channel_name: channel_name.to_string(),
            direction,
            description: description.to_string(),
            function: function.to_string(),
        }
    }
}

// ── Registry ────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct Registry {
    inner: s2n_quic_dc_metrics::Registry,
    queue_gauges: Arc<Mutex<HashMap<String, CachedQueueGauge>>>,
    queue_metadata: Arc<Mutex<HashMap<String, QueueRegistrationMetadata>>>,
    queue_endpoint_metadata: Arc<Mutex<HashMap<MetricId, QueueEndpointMetadata>>>,
    metric_metadata: Arc<Mutex<HashMap<MetricId, MetricMetadata>>>,
    topology: Arc<Mutex<Topology>>,
}

/// Runtime topology tracked by the counter registry.
///
/// This includes all tasks, channels/queues, and their bindings (sender/receiver relationships).
/// The registry owns this information and can expose it for runtime introspection (e.g., DOT graphs).
#[derive(Clone, Default, Debug)]
pub struct Topology {
    pub tasks: Vec<TaskRegistration>,
    pub channels: Vec<ChannelRegistration>,
    pub bindings: Vec<ChannelBinding>,
}

/// A Mermaid flowchart view of a [`Topology`] that can be rendered via [`Display`].
///
/// Obtained from [`Topology::to_mermaid`]. A custom [`MermaidMetricFormatter`] can be
/// supplied with [`Mermaid::with_metric_formatter`] before rendering.
///
/// # Type parameters
///
/// - `'a` — lifetime of the borrowed [`Topology`].
/// - `F` — metric formatter; defaults to [`DefaultMermaidMetricFormatter`].
///
/// [`Display`]: core::fmt::Display
pub struct Mermaid<'a, F = DefaultMermaidMetricFormatter> {
    topology: &'a Topology,
    formatter: F,
}

impl<'a> Mermaid<'a> {
    fn new(topology: &'a Topology) -> Self {
        Self {
            topology,
            formatter: DefaultMermaidMetricFormatter,
        }
    }
}

impl<'a, F> Mermaid<'a, F> {
    /// Replace the metric formatter, returning a new [`Mermaid`] with the given formatter.
    pub fn with_metric_formatter<G: MermaidMetricFormatter>(self, formatter: G) -> Mermaid<'a, G> {
        Mermaid {
            topology: self.topology,
            formatter,
        }
    }
}

impl<'a, F: MermaidMetricFormatter> core::fmt::Display for Mermaid<'a, F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let topology = self.topology;

        write!(
            f,
            "flowchart LR\n  classDef task_node fill:#eef2ff,stroke:#4f46e5,stroke-width:2px;\n  classDef channel_node fill:#ecfeff,stroke:#0891b2,stroke-width:2px;\n"
        )?;

        use std::collections::BTreeMap;
        let mut task_node_ids = std::collections::HashMap::new();
        let mut channel_node_ids = std::collections::HashMap::new();
        let mut worker_task_nodes = BTreeMap::<usize, Vec<String>>::new();
        let mut unassigned_task_nodes = Vec::new();

        // Render tasks
        for (idx, task) in topology.tasks.iter().enumerate() {
            let node_id = format!("t{idx}");
            task_node_ids.insert(task.name.clone(), node_id.clone());
            let budget = task
                .budget
                .map(|v| v.to_string())
                .unwrap_or_else(|| "none".to_string());
            let comments = metric_comment_lines(&self.formatter, &task.metrics);
            let metrics_display = format_metrics_with_formatter(&self.formatter, &task.metrics);
            let mut props = vec![
                format!("fn: {}", task.function),
                format!("budget: {budget}"),
            ];
            if let Some(metrics) = metrics_display {
                props.push(format!("metrics: {metrics}"));
            }
            props.push(format!("desc: {}", task.description));
            let label = format_mermaid_card(&task.name, props);
            let node_line = format!(
                "{node_id}[\"{}\"]\nclass {node_id} task_node;\n{comments}",
                label
            );

            if let Some(worker_id) = task.worker_id {
                worker_task_nodes
                    .entry(worker_id)
                    .or_default()
                    .push(node_line);
            } else {
                unassigned_task_nodes.push(node_line);
            }
        }

        for (worker_id, task_nodes) in worker_task_nodes {
            writeln!(f, "  subgraph worker_{worker_id}[worker {worker_id}]")?;
            for node in &task_nodes {
                write_indented(f, "    ", node)?;
            }
            writeln!(f, "  end")?;
        }

        for node in &unassigned_task_nodes {
            write_indented(f, "  ", node)?;
        }

        // Render channels
        for (idx, channel) in topology.channels.iter().enumerate() {
            let node_id = format!("c{idx}");
            channel_node_ids.insert(channel.name.clone(), node_id.clone());
            let comments = metric_comment_lines(&self.formatter, &channel.metrics);
            let metrics_display = format_metrics_with_formatter(&self.formatter, &channel.metrics);
            let mut props = vec![format!("fn: {}", channel.function)];
            if let Some(metrics) = metrics_display {
                props.push(format!("metrics: {metrics}"));
            }
            props.push(format!("desc: {}", channel.description));
            let label = format_mermaid_card(&channel.name, props);
            writeln!(f, "  {node_id}[\"{}\"]", label)?;
            writeln!(f, "  class {node_id} channel_node;")?;
            if !comments.is_empty() {
                write_indented(f, "  ", &comments)?;
            }
        }

        // Render bindings
        for binding in &topology.bindings {
            let Some(task_node) = task_node_ids.get(&binding.task_name) else {
                writeln!(
                    f,
                    "  %% unresolved binding: missing task '{}'",
                    binding.task_name
                )?;
                continue;
            };
            let Some(channel_node) = channel_node_ids.get(&binding.channel_name) else {
                writeln!(
                    f,
                    "  %% unresolved binding: missing channel '{}'",
                    binding.channel_name
                )?;
                continue;
            };

            let (from, to) = if binding.direction == ChannelDirection::Sends {
                (task_node, channel_node)
            } else {
                (channel_node, task_node)
            };
            let label = format!(
                "{}\nfn: {}\n{}",
                binding.direction, binding.function, binding.description
            );
            let edge_label = escape_mermaid(&label).replace('|', "&#124;");
            writeln!(f, "  {from} -->|{}| {to}", edge_label)?;
        }

        Ok(())
    }
}

impl Topology {
    /// Generate a DOT graph representation of the topology.
    pub fn to_dot(&self) -> String {
        let mut out = String::from("digraph pipeline {\n  rankdir=LR;\n");

        use std::collections::BTreeMap;
        let mut task_node_ids = std::collections::HashMap::new();
        let mut channel_node_ids = std::collections::HashMap::new();
        let mut worker_task_nodes = BTreeMap::<usize, Vec<String>>::new();
        let mut unassigned_task_nodes = Vec::new();

        // Render tasks
        for (idx, task) in self.tasks.iter().enumerate() {
            let node_id = format!("t{idx}");
            task_node_ids.insert(task.name.clone(), node_id.clone());
            let budget = task
                .budget
                .map(|v| v.to_string())
                .unwrap_or_else(|| "none".to_string());
            let metrics = metric_summary(&task.metrics);
            let label = format!(
                "{}\nfn: {}\nbudget: {}\nmetrics: {}\n{}",
                task.name, task.function, budget, metrics, task.description
            );
            let node_line = format!("{node_id} [shape=box,label=\"{}\"];\n", escape_dot(&label));

            if let Some(worker_id) = task.worker_id {
                worker_task_nodes
                    .entry(worker_id)
                    .or_default()
                    .push(node_line);
            } else {
                unassigned_task_nodes.push(node_line);
            }
        }

        for (worker_id, task_nodes) in worker_task_nodes {
            out.push_str(&format!(
                "  subgraph cluster_worker_{worker_id} {{\n    label=\"worker {worker_id}\";\n    style=rounded;\n"
            ));
            for node in task_nodes {
                out.push_str("    ");
                out.push_str(&node);
            }
            out.push_str("  }\n");
        }

        for node in unassigned_task_nodes {
            out.push_str("  ");
            out.push_str(&node);
        }

        // Render channels
        for (idx, channel) in self.channels.iter().enumerate() {
            let node_id = format!("c{idx}");
            channel_node_ids.insert(channel.name.clone(), node_id.clone());
            let metrics = metric_summary(&channel.metrics);
            let label = format!(
                "{}\nfn: {}\nmetrics: {}\n{}",
                channel.name, channel.function, metrics, channel.description
            );
            out.push_str(&format!(
                "  {node_id} [shape=ellipse,label=\"{}\"];\n",
                escape_dot(&label)
            ));
        }

        // Render bindings
        for binding in &self.bindings {
            let Some(task_node) = task_node_ids.get(&binding.task_name) else {
                out.push_str(&format!(
                    "  // unresolved binding: missing task '{}'\n",
                    binding.task_name
                ));
                continue;
            };
            let Some(channel_node) = channel_node_ids.get(&binding.channel_name) else {
                out.push_str(&format!(
                    "  // unresolved binding: missing channel '{}'\n",
                    binding.channel_name
                ));
                continue;
            };

            let (from, to) = if binding.direction == ChannelDirection::Sends {
                (task_node, channel_node)
            } else {
                (channel_node, task_node)
            };
            let label = format!(
                "{}\\nfn: {}\\n{}",
                binding.direction, binding.function, binding.description
            );
            out.push_str(&format!(
                "  {from} -> {to} [label=\"{}\"];\n",
                escape_dot(&label)
            ));
        }

        out.push_str("}\n");
        out
    }

    /// Returns a [`Mermaid`] view of this topology that renders as a Mermaid flowchart.
    ///
    /// The returned value implements [`Display`] and can be customized with a
    /// [`MermaidMetricFormatter`] via [`Mermaid::with_metric_formatter`].
    ///
    /// [`Display`]: core::fmt::Display
    pub fn to_mermaid(&self) -> Mermaid<'_> {
        Mermaid::new(self)
    }

    /// Generate a stable, human-readable snapshot representation of the topology.
    pub fn to_snapshot(&self) -> String {
        let mut out = String::new();

        let mut tasks = self.tasks.clone();
        tasks.sort_by(|a, b| {
            (
                a.name.as_str(),
                a.function.as_str(),
                a.description.as_str(),
                a.budget,
                a.worker_id,
            )
                .cmp(&(
                    b.name.as_str(),
                    b.function.as_str(),
                    b.description.as_str(),
                    b.budget,
                    b.worker_id,
                ))
        });
        out.push_str("tasks:\n");
        for task in tasks {
            out.push_str(&format!("  - name: {}\n", snapshot_text(&task.name)));
            out.push_str(&format!(
                "    function: {}\n",
                snapshot_text(&task.function)
            ));
            out.push_str(&format!(
                "    description: {}\n",
                snapshot_text(&task.description)
            ));
            out.push_str(&format!(
                "    budget: {}\n",
                task.budget
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "none".to_string())
            ));
            out.push_str(&format!(
                "    worker_id: {}\n",
                task.worker_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "none".to_string())
            ));
            write_metrics_snapshot(&mut out, &task.metrics);
        }

        let mut channels = self.channels.clone();
        channels.sort_by(|a, b| {
            (a.name.as_str(), a.function.as_str(), a.description.as_str()).cmp(&(
                b.name.as_str(),
                b.function.as_str(),
                b.description.as_str(),
            ))
        });
        out.push_str("channels:\n");
        for channel in channels {
            out.push_str(&format!("  - name: {}\n", snapshot_text(&channel.name)));
            out.push_str(&format!(
                "    function: {}\n",
                snapshot_text(&channel.function)
            ));
            out.push_str(&format!(
                "    description: {}\n",
                snapshot_text(&channel.description)
            ));
            write_metrics_snapshot(&mut out, &channel.metrics);
        }

        let mut bindings = self.bindings.clone();
        bindings.sort_by(|a, b| {
            (
                a.task_name.as_str(),
                a.channel_name.as_str(),
                direction_sort_key(a.direction),
                a.function.as_str(),
                a.description.as_str(),
            )
                .cmp(&(
                    b.task_name.as_str(),
                    b.channel_name.as_str(),
                    direction_sort_key(b.direction),
                    b.function.as_str(),
                    b.description.as_str(),
                ))
        });
        out.push_str("bindings:\n");
        for binding in bindings {
            out.push_str(&format!(
                "  - task: {}\n",
                snapshot_text(&binding.task_name)
            ));
            out.push_str(&format!(
                "    direction: {}\n",
                direction_sort_key(binding.direction)
            ));
            out.push_str(&format!(
                "    channel: {}\n",
                snapshot_text(&binding.channel_name)
            ));
            out.push_str(&format!(
                "    function: {}\n",
                snapshot_text(&binding.function)
            ));
            out.push_str(&format!(
                "    description: {}\n",
                snapshot_text(&binding.description)
            ));
        }

        out
    }

    /// Validate that the topology is coherent.
    ///
    /// Checks that every channel has at least one sender and one receiver.
    /// Returns a list of validation errors if any are found.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut channel_senders = std::collections::HashMap::new();
        let mut channel_receivers = std::collections::HashMap::new();

        // Count senders and receivers for each channel
        for binding in &self.bindings {
            match binding.direction {
                ChannelDirection::Sends => {
                    *channel_senders
                        .entry(binding.channel_name.clone())
                        .or_insert(0) += 1;
                }
                ChannelDirection::Receives => {
                    *channel_receivers
                        .entry(binding.channel_name.clone())
                        .or_insert(0) += 1;
                }
            }
        }

        // Check that every channel has at least one sender and one receiver
        for channel in &self.channels {
            let sender_count = channel_senders.get(&channel.name).copied().unwrap_or(0);
            let receiver_count = channel_receivers.get(&channel.name).copied().unwrap_or(0);

            if sender_count == 0 {
                errors.push(format!("Channel '{}' has no senders", channel.name));
            }
            if receiver_count == 0 {
                errors.push(format!("Channel '{}' has no receivers", channel.name));
            }
        }

        errors
    }
}

fn escape_dot(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn escape_mermaid(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "<br/>")
        .replace('\r', "")
}

fn format_mermaid_card(header: &str, properties: impl IntoIterator<Item = String>) -> String {
    let mut label = String::new();
    label.push_str(header);
    label.push_str("\n────────");
    for property in properties {
        label.push('\n');
        label.push_str(&property);
    }
    escape_mermaid(&label)
}

fn write_indented(
    f: &mut core::fmt::Formatter<'_>,
    indent: &str,
    block: &str,
) -> core::fmt::Result {
    for line in block.lines() {
        writeln!(f, "{indent}{line}")?;
    }
    Ok(())
}

fn snapshot_text(input: &str) -> String {
    input.replace('\n', "\\n").replace('\r', "\\r")
}

fn direction_sort_key(direction: ChannelDirection) -> &'static str {
    match direction {
        ChannelDirection::Sends => "sends",
        ChannelDirection::Receives => "receives",
    }
}

fn write_metrics_snapshot(out: &mut String, metrics: &[MetricRegistration]) {
    out.push_str("    metrics:\n");

    let mut metrics = metrics.to_vec();
    metrics.sort_by(|a, b| {
        (
            a.label.as_str(),
            a.variant.as_deref().unwrap_or(""),
            a.kind.to_string(),
            a.unit.unwrap_or(""),
            a.description.as_str(),
        )
            .cmp(&(
                b.label.as_str(),
                b.variant.as_deref().unwrap_or(""),
                b.kind.to_string(),
                b.unit.unwrap_or(""),
                b.description.as_str(),
            ))
    });

    if metrics.is_empty() {
        out.push_str("      - none\n");
        return;
    }

    for metric in metrics {
        out.push_str("      - ");
        out.push_str(&snapshot_text(&metric.label));
        out.push_str(" [");
        out.push_str(&metric.kind.to_string());
        if let Some(variant) = &metric.variant {
            out.push_str(" variant=");
            out.push_str(&snapshot_text(variant));
        }
        if let Some(unit) = metric.unit {
            out.push_str(" unit=");
            out.push_str(unit);
        }
        out.push_str("]: ");
        out.push_str(&snapshot_text(&metric.description));
        out.push('\n');
    }
}

fn metric_summary(metrics: &[MetricRegistration]) -> String {
    if metrics.is_empty() {
        "none".to_string()
    } else {
        metrics
            .iter()
            .map(|metric| {
                let variant = metric
                    .variant
                    .as_ref()
                    .map(|variant| format!(" variant={variant}"))
                    .unwrap_or_default();
                let unit = metric
                    .unit
                    .map(|unit| format!(" unit={unit}"))
                    .unwrap_or_default();
                format!(
                    "{} [{}{}{}]: {}",
                    metric.label, metric.kind, variant, unit, metric.description
                )
            })
            .collect::<Vec<_>>()
            .join("\\n")
    }
}

/// Render a metric list using the provided formatter for names while keeping
/// the standard unified format (`[kind variant unit]: description`) for the rest.
/// Returns `None` when the formatter hides all metrics (so callers can omit the
/// `metrics:` card line entirely).
fn format_metrics_with_formatter<F: MermaidMetricFormatter>(
    formatter: &F,
    metrics: &[MetricRegistration],
) -> Option<String> {
    if metrics.is_empty() {
        return Some("none".to_string());
    }
    let lines: Vec<String> = metrics
        .iter()
        .filter(|metric| formatter.should_display_metric(metric))
        .map(|metric| {
            let name = formatter.format_metric_name(metric);
            let variant = metric
                .variant
                .as_ref()
                .map(|variant| format!(" variant={variant}"))
                .unwrap_or_default();
            let unit = metric
                .unit
                .map(|unit| format!(" unit={unit}"))
                .unwrap_or_default();
            format!(
                "{name} [{}{}{}]: {}",
                metric.kind, variant, unit, metric.description
            )
        })
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\\n"))
    }
}

/// Emit well-structured `%% metric:` comments for a node, using the formatter's
/// `format_metric_name` so namespace prefixes and other transformations are reflected.
/// These comments are always emitted regardless of `should_display_metric` and are
/// machine-readable by custom Mermaid extensions.
fn metric_comment_lines<F: MermaidMetricFormatter>(
    formatter: &F,
    metrics: &[MetricRegistration],
) -> String {
    metrics
        .iter()
        .map(|metric| {
            let name = formatter.format_metric_name(metric);
            let variant = metric
                .variant
                .as_ref()
                .map(|variant| format!(" variant={variant}"))
                .unwrap_or_default();
            let unit = metric
                .unit
                .map(|unit| format!(" unit={unit}"))
                .unwrap_or_default();
            format!(
                "%% metric: {name} [{}{}{}]: {}\n",
                metric.kind, variant, unit, metric.description
            )
        })
        .collect()
}

impl Registry {
    const COUNT_UNIT: &'static str = "count";
    const MICROSECOND_UNIT: &'static str = "microsecond";

    fn unit_str(unit: Unit) -> Option<&'static str> {
        match unit {
            Unit::Count => Some("count"),
            Unit::Microsecond => Some("microsecond"),
            Unit::Byte => Some("byte"),
            Unit::Second => Some("second"),
            Unit::Percent => Some("percent"),
        }
    }

    pub fn new() -> Self {
        Self::with_inner(s2n_quic_dc_metrics::Registry::new())
    }

    /// Wraps an existing [`s2n_quic_dc_metrics::Registry`] instead of allocating a fresh one.
    ///
    /// The `inner` registry is what a reporter (see [`metrics`](Self::metrics)) actually drains, so
    /// an application that already owns a process-wide metrics registry can pass it here to have
    /// this endpoint record into it — its metrics then flow through the application's existing
    /// reporter alongside everything else, rather than needing a dedicated one. The wrapper-level
    /// metadata (queue gauges, topology, metric descriptions) always starts empty; it tracks only
    /// what is registered through this handle.
    pub fn with_inner(inner: s2n_quic_dc_metrics::Registry) -> Self {
        Self {
            inner,
            queue_gauges: Arc::new(Mutex::new(HashMap::new())),
            queue_metadata: Arc::new(Mutex::new(HashMap::new())),
            queue_endpoint_metadata: Arc::new(Mutex::new(HashMap::new())),
            metric_metadata: Arc::new(Mutex::new(HashMap::new())),
            topology: Arc::new(Mutex::new(Topology::default())),
        }
    }

    /// Wraps a [child](s2n_quic_dc_metrics::Registry::child) of this registry's underlying metrics
    /// registry, so every metric registered through the returned handle has its emitted name
    /// prefixed with `prefix`.
    ///
    /// This is how an application namespaces the transport's metrics relative to its own: the
    /// prefix is prepended to the metric name the backends emit (e.g. `rx.data` becomes
    /// `myapp.rx.data`), and because the child shares the parent's storage everything is reported
    /// together. Only the emitted name changes — the wrapper-level metadata and topology labels
    /// (used for tracing and graph rendering) keep the bare names. This composes with any
    /// reporter-level prefix the consumer applies when it emits the drained line.
    ///
    /// The returned handle starts with fresh wrapper-level metadata, tracking only what is
    /// registered through it (see [`with_inner`](Self::with_inner)).
    pub fn child(&self, prefix: impl AsRef<str>) -> Self {
        Self::with_inner(self.inner.child(prefix))
    }

    /// Returns a snapshot of the runtime topology tracked by this registry.
    ///
    /// This includes all registered tasks, channels, and their bindings.
    pub fn topology(&self) -> Topology {
        self.topology.lock().unwrap().clone()
    }

    /// Drains and returns the current metrics line (aggregating all per-CPU/fallback pages
    /// first), or `None` if nothing has been recorded. Test-only: lets a unit test assert that
    /// a specific metric (e.g. `stream.writer.sojourn[acked]`) was observed without snapshotting.
    #[cfg(test)]
    pub fn take_current_metrics_line(&self) -> Option<String> {
        self.inner.try_take_current_metrics_line_sparse(false)
    }

    /// Register a task in the topology.
    pub(crate) fn register_task_topology(&self, task: TaskRegistration) {
        self.topology.lock().unwrap().tasks.push(task);
    }

    /// Register a channel/queue in the topology.
    pub(crate) fn register_channel_topology(&self, channel: ChannelRegistration) {
        self.topology.lock().unwrap().channels.push(channel);
    }

    /// Update or insert a channel/queue in the topology by name.
    pub(crate) fn upsert_channel_topology(&self, channel: ChannelRegistration) {
        let mut topology = self.topology.lock().unwrap();
        if let Some(existing) = topology
            .channels
            .iter_mut()
            .find(|c| c.name == channel.name)
        {
            *existing = channel;
        } else {
            topology.channels.push(channel);
        }
    }

    /// Register a channel binding (sender/receiver relationship) in the topology.
    pub(crate) fn register_binding_topology(&self, binding: ChannelBinding) {
        self.topology.lock().unwrap().bindings.push(binding);
    }

    /// Update or insert a channel binding in the topology.
    /// Matches on task_name, channel_name, and direction.
    pub(crate) fn upsert_binding_topology(&self, binding: ChannelBinding) {
        let mut topology = self.topology.lock().unwrap();
        if let Some(existing) = topology.bindings.iter_mut().find(|b| {
            b.task_name == binding.task_name
                && b.channel_name == binding.channel_name
                && b.direction == binding.direction
        }) {
            *existing = binding;
        } else {
            topology.bindings.push(binding);
        }
    }

    /// Register a queue/channel in the topology.
    pub fn register_queue_channel(&self, channel: &QueueGauge) {
        let registration = channel.with_registration_metadata_ref(|name, description, function| {
            ChannelRegistration::new(name, description, function).with_metric(channel)
        });
        self.register_channel_topology(registration);
    }

    /// Register a queue sender in the topology.
    pub fn register_queue_sender(&self, sender: &QueueSender) {
        let channel_name = sender.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            sender.task_name(),
            channel_name,
            ChannelDirection::Sends,
            sender.description(),
            sender.function(),
        );
        self.register_binding_topology(binding);
    }

    /// Register a queue receiver in the topology.
    pub fn register_queue_receiver(&self, receiver: &QueueReceiver) {
        let channel_name = receiver.channel_metadata(|name, _, _| name.to_string());
        let binding = ChannelBinding::new(
            receiver.task_name(),
            channel_name,
            ChannelDirection::Receives,
            receiver.description(),
            receiver.function(),
        );
        self.register_binding_topology(binding);
    }

    fn register_metric_metadata(
        &self,
        label: impl core::fmt::Display,
        variant: Option<&str>,
        kind: MetricKind,
        unit: Option<&'static str>,
        description: impl core::fmt::Display,
    ) -> MetricId {
        let mut metric_metadata = self.metric_metadata.lock().unwrap();
        // Metric metadata entries are append-only for the lifetime of the registry,
        // so deriving the next id from current length preserves uniqueness.
        let id = metric_metadata.len() as MetricId + 1;
        metric_metadata.insert(
            id,
            MetricMetadata {
                id,
                label: label.to_string(),
                variant: variant.map(|variant| variant.to_string()),
                kind,
                unit,
                description: description.to_string(),
            },
        );
        id
    }

    fn counter_handle(&self, inner: s2n_quic_dc_metrics::Counter, metric_id: MetricId) -> Counter {
        Counter {
            inner,
            metric_id,
            metadata: self.metric_metadata.clone(),
        }
    }

    fn gauge_handle(&self, inner: Arc<AtomicI64>, metric_id: MetricId) -> Gauge {
        Gauge {
            inner,
            metric_id,
            metadata: self.metric_metadata.clone(),
        }
    }

    fn timer_handle(&self, summary: RawSummary, metric_id: MetricId) -> Timer {
        Timer {
            summary,
            metric_id,
            metadata: self.metric_metadata.clone(),
            sampler: Sampler::new(),
            sample_mask: Timer::DEFAULT_SAMPLE_MASK,
        }
    }

    fn summary_handle(&self, summary: RawSummary, metric_id: MetricId) -> Summary {
        Summary {
            summary,
            metric_id,
            metadata: self.metric_metadata.clone(),
        }
    }

    pub fn register(&self, label: impl core::fmt::Display) -> Counter {
        let label = label.to_string();
        let metric_id = self.register_metric_metadata(&label, None, MetricKind::Counter, None, "");
        self.counter_handle(self.inner.register_counter(label, None), metric_id)
    }

    pub fn register_bytes(&self, label: impl core::fmt::Display) -> Counter {
        let label = label.to_string();
        let metric_id =
            self.register_metric_metadata(&label, None, MetricKind::Counter, Some("B"), "");
        // Carry the byte unit as a real `Unit::Byte` rather than smuggling it through the
        // aggregation/variant dimension. The querylog line still renders the trailing ` B` (via the
        // unit's display suffix), while statsd no longer emits a spurious `|#variant:B` tag for what
        // was never a variant.
        self.counter_handle(
            self.inner
                .register_counter_with_unit(label, None, Unit::Byte),
            metric_id,
        )
    }

    pub fn register_nominal(
        &self,
        label: impl core::fmt::Display,
        variant: impl core::fmt::Display,
    ) -> Counter {
        let label = label.to_string();
        let variant = variant.to_string();
        let metric_id =
            self.register_metric_metadata(&label, Some(&variant), MetricKind::Counter, None, "");
        self.counter_handle(
            self.inner
                .register_counter(label, Some(variant_aggregation(&variant))),
            metric_id,
        )
    }

    pub fn register_queue_gauge(&self, label: impl core::fmt::Display) -> QueueGauge {
        let label = label.to_string();
        let mut gauges = self.queue_gauges.lock().unwrap();
        if let Some(existing) = gauges.get(&label) {
            return existing.gauge(self);
        }

        let enqueued_id = self.register_metric_metadata(
            format!("{label}.enq"),
            None,
            MetricKind::Counter,
            Some(Self::COUNT_UNIT),
            QueueGauge::ENQUEUED_DESCRIPTION,
        );
        let throughput = self.counter_handle(
            self.inner.register_counter(format!("{label}.enq"), None),
            enqueued_id,
        );
        let dequeued_id = self.register_metric_metadata(
            format!("{label}.drain"),
            None,
            MetricKind::Counter,
            Some(Self::COUNT_UNIT),
            QueueGauge::DEQUEUED_DESCRIPTION,
        );
        let drain = self.counter_handle(
            self.inner.register_counter(format!("{label}.drain"), None),
            dequeued_id,
        );
        let depth_inner = Arc::new(AtomicI64::new(0));
        let depth_clone = depth_inner.clone();
        self.inner.register_list_callback_zero_suppressed(
            format!("{label}.depth"),
            None,
            Unit::Count,
            // Floor the published value at 0: the internal counter can transiently read negative
            // when dequeues outrun counted enqueues, and a negative gauge is rejected downstream.
            // Only the emitted value is clamped; the internal atomic is left untouched.
            move || depth_clone.load(Ordering::Relaxed).max(0),
        );
        let depth_id = self.register_metric_metadata(
            format!("{label}.depth"),
            None,
            MetricKind::Gauge,
            Some(Self::COUNT_UNIT),
            QueueGauge::DEPTH_DESCRIPTION,
        );
        let depth_distribution_inner =
            self.inner
                .register_summary(format!("{label}.depth_dist"), None, Unit::Count);
        let depth_distribution_id = self.register_metric_metadata(
            format!("{label}.depth_dist"),
            None,
            MetricKind::Summary,
            Some(Self::COUNT_UNIT),
            QueueGauge::DEPTH_DISTRIBUTION_DESCRIPTION,
        );

        let gauge = QueueGauge {
            registry: self.clone(),
            key: label.clone(),
            throughput,
            drain,
            depth: self.gauge_handle(depth_inner, depth_id),
            depth_distribution: self
                .summary_handle(depth_distribution_inner, depth_distribution_id),
        };
        self.queue_metadata.lock().unwrap().insert(
            label.clone(),
            QueueRegistrationMetadata {
                label: label.clone(),
                variant: None,
                name: label.clone(),
                ..Default::default()
            },
        );

        // Auto-register in topology
        let registration = gauge.with_registration_metadata_ref(|name, description, function| {
            ChannelRegistration::new(name, description, function).with_metric(&gauge)
        });
        self.register_channel_topology(registration);

        gauges.insert(label, CachedQueueGauge::new(&gauge));
        gauge
    }

    pub fn register_queue_gauge_nominal(
        &self,
        label: impl core::fmt::Display,
        variant: impl core::fmt::Display,
    ) -> QueueGauge {
        let label = label.to_string();
        let variant = variant.to_string();
        let key = format!("{label}.{variant}");
        let mut gauges = self.queue_gauges.lock().unwrap();
        if let Some(existing) = gauges.get(&key) {
            return existing.gauge(self);
        }

        // Raw `variant` stays in the dc metadata (topology/trace display); the metrics crate records
        // the keyed `Variant|<variant>` aggregation.
        let variant_option = Some(variant_aggregation(&variant));
        let enqueued_id = self.register_metric_metadata(
            format!("{label}.enq"),
            Some(&variant),
            MetricKind::Counter,
            Some(Self::COUNT_UNIT),
            QueueGauge::ENQUEUED_DESCRIPTION,
        );
        let throughput = self.counter_handle(
            self.inner
                .register_counter(format!("{label}.enq"), variant_option.clone()),
            enqueued_id,
        );
        let dequeued_id = self.register_metric_metadata(
            format!("{label}.drain"),
            Some(&variant),
            MetricKind::Counter,
            Some(Self::COUNT_UNIT),
            QueueGauge::DEQUEUED_DESCRIPTION,
        );
        let drain = self.counter_handle(
            self.inner
                .register_counter(format!("{label}.drain"), variant_option.clone()),
            dequeued_id,
        );
        let depth_inner = Arc::new(AtomicI64::new(0));
        let depth_clone = depth_inner.clone();
        self.inner.register_list_callback_zero_suppressed(
            format!("{label}.depth"),
            variant_option.clone(),
            Unit::Count,
            // Floor the published value at 0: the internal counter can transiently read negative
            // when dequeues outrun counted enqueues, and a negative gauge is rejected downstream.
            // Only the emitted value is clamped; the internal atomic is left untouched.
            move || depth_clone.load(Ordering::Relaxed).max(0),
        );
        let depth_id = self.register_metric_metadata(
            format!("{label}.depth"),
            Some(&variant),
            MetricKind::Gauge,
            Some(Self::COUNT_UNIT),
            QueueGauge::DEPTH_DESCRIPTION,
        );
        let depth_distribution_inner =
            self.inner
                .register_summary(format!("{label}.depth_dist"), variant_option, Unit::Count);
        let depth_distribution_id = self.register_metric_metadata(
            format!("{label}.depth_dist"),
            Some(&variant),
            MetricKind::Summary,
            Some(Self::COUNT_UNIT),
            QueueGauge::DEPTH_DISTRIBUTION_DESCRIPTION,
        );

        let gauge = QueueGauge {
            registry: self.clone(),
            key: key.clone(),
            throughput,
            drain,
            depth: self.gauge_handle(depth_inner, depth_id),
            depth_distribution: self
                .summary_handle(depth_distribution_inner, depth_distribution_id),
        };
        self.queue_metadata.lock().unwrap().insert(
            key.clone(),
            QueueRegistrationMetadata {
                label: label.clone(),
                variant: Some(variant.clone()),
                name: key.clone(),
                ..Default::default()
            },
        );

        // Auto-register in topology
        let registration = gauge.with_registration_metadata_ref(|name, description, function| {
            ChannelRegistration::new(name, description, function).with_metric(&gauge)
        });
        self.register_channel_topology(registration);

        gauges.insert(key, CachedQueueGauge::new(&gauge));
        gauge
    }

    pub fn register_gauge(&self, label: impl core::fmt::Display) -> Gauge {
        let label = label.to_string();
        let metric_id = self.register_metric_metadata(&label, None, MetricKind::Gauge, None, "");
        let inner = Arc::new(AtomicI64::new(0));
        let inner_clone = inner.clone();
        self.inner
            .register_list_callback_zero_suppressed(label, None, Unit::Count, move || {
                inner_clone.load(Ordering::Relaxed)
            });
        self.gauge_handle(inner, metric_id)
    }

    /// Register a **pull-based** gauge: the supplied closure is invoked by the reporter once per
    /// emit interval to produce the current value, instead of the caller pushing updates into an
    /// owned atomic. Use this when the value is cheap to compute on demand and pushing every
    /// change would be wasteful — e.g. a value derived from atomics that mutate millions of times
    /// per interval, where only the sampled snapshot matters.
    ///
    /// There is no returned handle: the gauge has no backing store the caller writes to. A value
    /// of zero is suppressed from the metrics output (same as [`register_gauge`]).
    ///
    /// [`register_gauge`]: Self::register_gauge
    pub fn register_gauge_callback<F>(&self, label: impl core::fmt::Display, f: F)
    where
        F: FnMut() -> i64 + 'static + Send,
    {
        let label = label.to_string();
        self.register_metric_metadata(&label, None, MetricKind::Gauge, None, "");
        self.inner
            .register_list_callback_zero_suppressed(label, None, Unit::Count, f);
    }

    pub fn register_nominal_gauge(
        &self,
        label: impl core::fmt::Display,
        variant: impl core::fmt::Display,
    ) -> Gauge {
        let label = label.to_string();
        let variant = variant.to_string();
        let metric_id =
            self.register_metric_metadata(&label, Some(&variant), MetricKind::Gauge, None, "");
        let inner = Arc::new(AtomicI64::new(0));
        let inner_clone = inner.clone();
        self.inner.register_list_callback_zero_suppressed(
            label,
            Some(variant_aggregation(&variant)),
            Unit::Count,
            move || inner_clone.load(Ordering::Relaxed),
        );
        self.gauge_handle(inner, metric_id)
    }

    /// Register a **pull-based** gauge under a variant: the supplied closure is invoked by the
    /// reporter once per emit interval and its value is reported under `label` with the
    /// aggregation `Variant|variant`, as [`register_nominal_gauge`] reports a pushed value. Use it
    /// where [`register_gauge_callback`] fits but the value is one of a family, such as a
    /// per-worker measure.
    ///
    /// There is no returned handle: the gauge has no backing store the caller writes to. A value
    /// of zero is suppressed from the metrics output (same as [`register_gauge`]).
    ///
    /// [`register_nominal_gauge`]: Self::register_nominal_gauge
    /// [`register_gauge_callback`]: Self::register_gauge_callback
    /// [`register_gauge`]: Self::register_gauge
    pub fn register_nominal_gauge_callback<F>(
        &self,
        label: impl core::fmt::Display,
        variant: impl core::fmt::Display,
        f: F,
    ) where
        F: FnMut() -> i64 + 'static + Send,
    {
        let label = label.to_string();
        let variant = variant.to_string();
        self.register_metric_metadata(&label, Some(&variant), MetricKind::Gauge, None, "");
        self.inner.register_list_callback_zero_suppressed(
            label,
            Some(variant_aggregation(&variant)),
            Unit::Count,
            f,
        );
    }

    pub fn register_summary(&self, label: impl core::fmt::Display, unit: Unit) -> Summary {
        let label = label.to_string();
        let metric_id = self.register_metric_metadata(
            &label,
            None,
            MetricKind::Summary,
            Self::unit_str(unit),
            "",
        );
        let summary = self.inner.register_summary(label, None, unit);
        self.summary_handle(summary, metric_id)
    }

    pub fn register_nominal_summary(
        &self,
        label: impl core::fmt::Display,
        variant: impl core::fmt::Display,
        unit: Unit,
    ) -> Summary {
        let label = label.to_string();
        let variant = variant.to_string();
        let metric_id = self.register_metric_metadata(
            &label,
            Some(&variant),
            MetricKind::Summary,
            Self::unit_str(unit),
            "",
        );
        let summary = self
            .inner
            .register_summary(label, Some(variant_aggregation(&variant)), unit);
        self.summary_handle(summary, metric_id)
    }

    pub fn register_timer(&self, label: impl core::fmt::Display) -> Timer {
        let label = label.to_string();
        let metric_id = self.register_metric_metadata(
            &label,
            None,
            MetricKind::Timer,
            Some(Self::MICROSECOND_UNIT),
            "",
        );
        self.timer_handle(
            self.inner.register_summary(label, None, Unit::Microsecond),
            metric_id,
        )
    }

    pub fn register_task(&self, label: impl core::fmt::Display) -> Task {
        let label = label.to_string();
        let drained_label = format!("{label}.drained");
        let time_label = format!("{label}.time");
        let next_poll_latency_label = format!("{label}.next_poll_latency");
        let time_id = self.register_metric_metadata(
            &time_label,
            None,
            MetricKind::Timer,
            Some(Self::MICROSECOND_UNIT),
            Task::TIME_DESCRIPTION,
        );
        let next_poll_latency_id = self.register_metric_metadata(
            &next_poll_latency_label,
            None,
            MetricKind::Timer,
            Some(Self::MICROSECOND_UNIT),
            Task::NEXT_POLL_LATENCY_DESCRIPTION,
        );
        Task {
            registry: self.clone(),
            registration: Arc::new(Mutex::new(TaskRegistrationMetadata {
                name: label.clone(),
                ..Default::default()
            })),
            drained: self
                .register_summary(drained_label, Unit::Count)
                .with_description(Task::DRAINED_DESCRIPTION),
            time: self.timer_handle(
                self.inner
                    .register_summary(time_label, None, Unit::Microsecond),
                time_id,
            ),
            next_poll_latency: self.timer_handle(
                self.inner
                    .register_summary(next_poll_latency_label, None, Unit::Microsecond),
                next_poll_latency_id,
            ),
        }
    }

    pub fn register_nominal_timer(
        &self,
        label: impl core::fmt::Display,
        variant: impl core::fmt::Display,
    ) -> Timer {
        let label = label.to_string();
        let variant = variant.to_string();
        let metric_id = self.register_metric_metadata(
            &label,
            Some(&variant),
            MetricKind::Timer,
            Some(Self::MICROSECOND_UNIT),
            "",
        );
        self.timer_handle(
            self.inner.register_summary(
                label,
                Some(variant_aggregation(&variant)),
                Unit::Microsecond,
            ),
            metric_id,
        )
    }

    pub fn register_nominal_task(
        &self,
        label: impl core::fmt::Display,
        variant: impl core::fmt::Display,
    ) -> Task {
        let label = label.to_string();
        let variant = variant.to_string();
        let drained_label = format!("{label}.drained");
        let time_label = format!("{label}.time");
        let next_poll_latency_label = format!("{label}.next_poll_latency");
        let time_id = self.register_metric_metadata(
            &time_label,
            Some(&variant),
            MetricKind::Timer,
            Some(Self::MICROSECOND_UNIT),
            Task::TIME_DESCRIPTION,
        );
        let next_poll_latency_id = self.register_metric_metadata(
            &next_poll_latency_label,
            Some(&variant),
            MetricKind::Timer,
            Some(Self::MICROSECOND_UNIT),
            Task::NEXT_POLL_LATENCY_DESCRIPTION,
        );
        Task {
            registry: self.clone(),
            registration: Arc::new(Mutex::new(TaskRegistrationMetadata {
                name: format!("{label}.{variant}"),
                ..Default::default()
            })),
            drained: self
                .register_nominal_summary(drained_label, &variant, Unit::Count)
                .with_description(Task::DRAINED_DESCRIPTION),
            time: self.timer_handle(
                self.inner.register_summary(
                    time_label,
                    Some(variant_aggregation(&variant)),
                    Unit::Microsecond,
                ),
                time_id,
            ),
            next_poll_latency: self.timer_handle(
                self.inner.register_summary(
                    next_poll_latency_label,
                    Some(variant_aggregation(&variant)),
                    Unit::Microsecond,
                ),
                next_poll_latency_id,
            ),
        }
    }

    /// The underlying [`s2n_quic_dc_metrics::Registry`] this handle records into.
    ///
    /// This is the registry an exporting reporter drives: call
    /// [`report_with`](s2n_quic_dc_metrics::Registry::report_with) on it to drain the metric pool
    /// into a [`Backend`](s2n_quic_dc_metrics::Backend) each interval, and
    /// [`is_open`](s2n_quic_dc_metrics::Registry::is_open) to stop when the endpoint shuts down. The
    /// crate itself no longer ships a reporter — a consumer owns its own export loop (or uses
    /// [`spawn_noop_drain_reporter`] purely to bound the pool).
    pub fn metrics(&self) -> &s2n_quic_dc_metrics::Registry {
        &self.inner
    }
}

// ── GaugedQueueReceiver ─────────────────────────────────────────────────────────────

pub struct GaugedQueueReceiver<T, R> {
    inner: R,
    queue: crate::intrusive::Queue<T>,
    gauge: QueueReceiver,
}

impl<T, R> GaugedQueueReceiver<T, R> {
    pub fn new(inner: R, gauge: QueueReceiver) -> Self {
        Self {
            inner,
            queue: Default::default(),
            gauge,
        }
    }
}

impl<T, R> crate::socket::channel::Receiver<crate::intrusive::Entry<T>>
    for GaugedQueueReceiver<T, R>
where
    R: crate::socket::channel::Receiver<crate::intrusive::Queue<T>>,
{
    fn poll_recv(
        &mut self,
        cx: &mut core::task::Context<'_>,
        budget: &mut crate::socket::channel::Budget,
    ) -> core::task::Poll<Option<crate::intrusive::Entry<T>>> {
        loop {
            if budget.is_exhausted() {
                if !self.queue.is_empty() {
                    budget.set_needs_wake();
                }
                return core::task::Poll::Pending;
            }

            if let Some(entry) = self.queue.pop_front() {
                self.gauge.on_receive(1);
                budget.consume();
                return core::task::Poll::Ready(Some(entry));
            }

            match self.inner.poll_recv(cx, budget) {
                core::task::Poll::Ready(Some(queue)) => {
                    if queue.is_empty() {
                        budget.set_needs_wake();
                        return core::task::Poll::Pending;
                    }
                    self.queue = queue;
                }
                core::task::Poll::Ready(None) => return core::task::Poll::Ready(None),
                core::task::Poll::Pending => return core::task::Poll::Pending,
            }
        }
    }

    fn on_consumed(&mut self, bytes: u64) {
        self.inner.on_consumed(bytes);
    }
}

// ── GaugedSender ────────────────────────────────────────────────────────────

pub struct GaugedSender<S> {
    inner: S,
    gauge: QueueSender,
}

impl<S> GaugedSender<S> {
    pub fn new(inner: S, gauge: QueueSender) -> Self {
        Self { inner, gauge }
    }
}

impl<S: Clone> Clone for GaugedSender<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            gauge: self.gauge.clone(),
        }
    }
}

impl<T, S> crate::socket::channel::UnboundedSender<T> for GaugedSender<S>
where
    S: crate::socket::channel::UnboundedSender<T>,
{
    fn send(&mut self, value: T) -> Result<(), T> {
        match self.inner.send(value) {
            Ok(()) => {
                self.gauge.on_send(1);
                Ok(())
            }
            Err(v) => Err(v),
        }
    }
}

// ── GaugedReceiver ──────────────────────────────────────────────────────────

pub struct GaugedReceiver<R> {
    inner: R,
    gauge: QueueReceiver,
}

impl<R> GaugedReceiver<R> {
    pub fn new(inner: R, gauge: QueueReceiver) -> Self {
        Self { inner, gauge }
    }
}

impl<T, R> crate::socket::channel::Receiver<T> for GaugedReceiver<R>
where
    R: crate::socket::channel::Receiver<T>,
{
    fn poll_recv(
        &mut self,
        cx: &mut core::task::Context<'_>,
        budget: &mut crate::socket::channel::Budget,
    ) -> core::task::Poll<Option<T>> {
        match self.inner.poll_recv(cx, budget) {
            core::task::Poll::Ready(Some(v)) => {
                self.gauge.on_receive(1);
                core::task::Poll::Ready(Some(v))
            }
            other => other,
        }
    }

    fn on_consumed(&mut self, bytes: u64) {
        self.inner.on_consumed(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry and its queue gauges drop together. Each `QueueGauge` holds a handle on its
    /// registry, so a registry that cached whole gauges by name held itself alive, with every
    /// metric registered through it.
    #[test]
    fn registry_with_queue_gauges_drops() {
        let registry = Registry::new();
        let gauge = registry.register_queue_gauge("q.plain");
        let nominal = registry.register_queue_gauge_nominal("q.nominal", "send.worker.1");
        // A second registration of the same queue answers from the cache.
        let cached = registry.register_queue_gauge("q.plain");
        let alive = Arc::downgrade(&registry.queue_gauges);
        drop((registry, gauge, nominal, cached));
        assert!(alive.upgrade().is_none(), "a dropped registry stayed alive");
    }

    /// Every nominal dimension is emitted as a single `Variant|<value>` aggregation on the querylog
    /// line — the convention the querylog consumer expects for all variants — across counters,
    /// summaries, timers, gauges, and queue-gauge/endpoint counters. dc-side metric metadata keeps
    /// the raw value.
    #[test]
    fn nominal_gauge_callback_emits_variant_key() {
        let inner = s2n_quic_dc_metrics::Registry::new();
        let registry = Registry::with_inner(inner.clone());

        let mut calls = 0;
        registry.register_nominal_gauge_callback("pool.rate", "worker.3", move || {
            calls += 1;
            calls * 10
        });
        // A zero is suppressed, as for `register_gauge_callback`.
        registry.register_nominal_gauge_callback("pool.zero", "worker.3", || 0);

        let line = inner
            .try_take_current_metrics_line_sparse(true)
            .expect("metrics were recorded");
        assert!(
            line.contains("pool.rate=10 Variant|worker.3"),
            "got: {line}"
        );
        assert!(!line.contains("pool.zero"), "zero not suppressed: {line}");

        // The closure is called again on each emit, so a stateful rate calculation works.
        let line = inner
            .try_take_current_metrics_line_sparse(true)
            .expect("metrics were recorded");
        assert!(
            line.contains("pool.rate=20 Variant|worker.3"),
            "got: {line}"
        );
    }

    #[test]
    fn nominal_dimensions_emit_variant_key() {
        let inner = s2n_quic_dc_metrics::Registry::new();
        let registry = Registry::with_inner(inner.clone());

        // A semantic variant and a per-worker index both key as `Variant|...`.
        registry.register_nominal("rx.ecn", "ect0").add(1);
        registry.register_nominal("!send.lost", "send.5").add(1);
        registry
            .register_nominal_summary("send.cwnd", "send.5", Unit::Byte)
            .record_value(1000);
        // A per-worker queue gauge + its endpoint counter.
        let q = registry.register_queue_gauge_nominal("q.resolver", "send.worker.2");
        q.enqueue(1);
        let _sender = q.sender("task.context_resolver");
        q.enqueue(1); // ensure the endpoint counter is exercised via a send below
        q.sender("task.context_resolver"); // idempotent handle; drives .sender counter registration

        let line = inner
            .try_take_current_metrics_line_sparse(true)
            .expect("metrics were recorded");

        // Semantic + worker dimensions are both keyed `Variant|<value>` (no bare dims, no `worker|`).
        assert!(line.contains("rx.ecn=1 Variant|ect0"), "got: {line}");
        assert!(line.contains("!send.lost=1 Variant|send.5"), "got: {line}");
        assert!(
            line.contains("send.cwnd=") && line.contains("Variant|send.5"),
            "got: {line}"
        );
        assert!(
            line.contains("q.resolver.enq=") && line.contains("Variant|send.worker.2"),
            "got: {line}"
        );
        // The queue's endpoint counter carries the queue's worker variant (single dimension), so it
        // collapses with the rest of that worker's queue metrics.
        assert!(
            line.contains("q.resolver.sender=") && line.contains("Variant|send.worker.2"),
            "got: {line}"
        );
        // No bare (unkeyed) or `worker|`-keyed dimensions leak onto the line.
        assert!(!line.contains("worker|"), "unexpected worker| key: {line}");
        assert!(
            !line.contains(" send.5") && !line.contains(" ect0,") && !line.contains(" ect0\n"),
            "unexpected bare dimension: {line}"
        );
    }

    /// The internal depth counter can drift negative when dequeues outrun counted enqueues, but the
    /// *published* gauge must never be negative (a negative value is rejected by some downstream
    /// consumers, silencing the whole gauge). The report callback floors at 0, so an over-drained
    /// depth publishes as a suppressed zero rather than a negative number.
    #[test]
    fn depth_gauge_floors_negative_at_zero() {
        for variant in [None, Some("send.5")] {
            let inner = s2n_quic_dc_metrics::Registry::new();
            let registry = Registry::with_inner(inner.clone());

            let q = match variant {
                None => registry.register_queue_gauge("q.floor_test"),
                Some(v) => registry.register_queue_gauge_nominal("q.floor_test", v),
            };

            // Drive the internal atomic negative: 1 counted enqueue, 3 dequeues.
            q.enqueue(1);
            q.dequeue_n(3);

            let line = inner
                .try_take_current_metrics_line_sparse(true)
                .expect("metrics were recorded");

            // The published depth is floored: a negative value never reaches the wire (it renders as
            // `depth=-N` when the callback returns the raw atomic).
            assert!(
                !line.contains("depth=-"),
                "negative depth published (variant {variant:?}): {line}"
            );
        }
    }

    #[test]
    fn child_prefixes_emitted_metric_names() {
        let inner = s2n_quic_dc_metrics::Registry::new();
        let registry = Registry::with_inner(inner.clone());
        let child = registry.child("myapp");

        registry.register("rx.data").add(1);
        child.register("rx.data").add(2);
        // A metric registered through a queue-gauge clone of the child is namespaced too, since the
        // child's inner registry threads through the clone.
        child.register_queue_gauge("q.work").enqueue(1);

        let line = inner
            .try_take_current_metrics_line_sparse(true)
            .expect("metrics were recorded");
        assert!(line.contains("myapp.rx.data=2"), "got: {line}");
        assert!(line.contains("myapp.q.work.enq"), "got: {line}");
        assert!(line.contains("myapp.q.work.depth"), "got: {line}");
        // The parent's own metric keeps its bare name.
        assert!(
            line.contains(",rx.data=1") || line.starts_with("rx.data=1"),
            "got: {line}"
        );
    }

    #[test]
    fn topology_validation() {
        let registry = Registry::new();

        // Create a queue with both sender and receiver
        let q1 = registry.register_queue_gauge("q.test1");
        let _sender1 = q1.sender("task.sender");
        let _receiver1 = q1.receiver("task.receiver");

        // Create a queue with only a sender (missing receiver)
        let q2 = registry.register_queue_gauge("q.test2");
        let _sender2 = q2.sender("task.sender");

        // Create a queue with only a receiver (missing sender)
        let q3 = registry.register_queue_gauge("q.test3");
        let _receiver3 = q3.receiver("task.receiver");

        let topology = registry.topology();
        let errors = topology.validate();

        // Should have 2 errors: q.test2 missing receiver, q.test3 missing sender
        assert_eq!(errors.len(), 2);
        assert!(errors
            .iter()
            .any(|e| e.contains("q.test2") && e.contains("no receivers")));
        assert!(errors
            .iter()
            .any(|e| e.contains("q.test3") && e.contains("no senders")));
    }

    #[test]
    fn topology_dot_groups_tasks_by_worker_id() {
        let topology = Topology {
            tasks: vec![
                TaskRegistration::new("task.worker.1", "desc", "fn").with_worker_id(Some(1)),
                TaskRegistration::new("task.worker.2", "desc", "fn").with_worker_id(Some(2)),
                TaskRegistration::new("task.unassigned", "desc", "fn"),
            ],
            channels: vec![],
            bindings: vec![],
        };

        insta::assert_snapshot!(topology.to_dot());
    }

    #[test]
    fn topology_mermaid_groups_tasks_by_worker_id() {
        let topology = Topology {
            tasks: vec![
                TaskRegistration::new("task.worker.1", "desc", "fn").with_worker_id(Some(1)),
                TaskRegistration::new("task.worker.2", "desc", "fn").with_worker_id(Some(2)),
                TaskRegistration::new("task.unassigned", "desc", "fn"),
            ],
            channels: vec![ChannelRegistration::new(
                "queue.main",
                "queue desc",
                "queue_fn",
            )],
            bindings: vec![
                ChannelBinding::new(
                    "task.worker.1",
                    "queue.main",
                    ChannelDirection::Sends,
                    "write path",
                    "worker_send",
                ),
                ChannelBinding::new(
                    "task.worker.2",
                    "queue.main",
                    ChannelDirection::Receives,
                    "read path",
                    "worker_recv",
                ),
                ChannelBinding::new(
                    "task.unassigned",
                    "queue.main",
                    ChannelDirection::Receives,
                    "monitor path",
                    "monitor_recv",
                ),
            ],
        };

        insta::assert_snapshot!(topology.to_mermaid());
    }
}
