// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! A pull-based [`Backend`] that maintains cumulative Prometheus series in memory and renders the
//! [text exposition format] on demand.
//!
//! # Why this backend is different
//!
//! Unlike the statsd/querylog backends, which format each report's values and forward them
//! immediately, Prometheus is **pull-based** and its counters/histograms must be **cumulative and
//! monotonic**. But [`Registry::report`](crate::Registry::report) is *destructive*: it drains each
//! counter and histogram to zero every pass, handing the backend only the delta since the last
//! report. This backend therefore keeps its own authoritative, cumulative state and *accumulates*
//! each delta into it:
//!
//! - counters add the delta into a running total;
//! - gauges and callbacks (instantaneous readings) take the latest value (last-write-wins);
//! - histograms accumulate per-bucket counts, plus a running sum and observation count.
//!
//! A scrape endpoint reads the state through a [`PrometheusHandle`]. To keep the scrape path cheap
//! and off the reporting hot path, the backend re-encodes the full snapshot into the exposition
//! string once per report (in [`report_end`](Backend::report_end)) and publishes it behind a shared
//! cell; `encode` just clones the published `Arc<str>`. Values only change at report boundaries
//! anyway (that is when the registry drains), so a per-report snapshot loses no fidelity.
//!
//! # Name and label mapping
//!
//! Metric names are sanitized to the Prometheus grammar (`[a-zA-Z0-9_:]`, e.g. `rx.data` ->
//! `rx_data`). The historical `aggregation` convention string becomes labels: `Variant|ect0` ->
//! `{variant="ect0"}`, `Task|foo` -> `{task="foo"}`, and a bare string with no `|` -> its own
//! `aggregation` label. Bool counters collapse into a single counter family with a `result="true"`
//! / `result="false"` label.
//!
//! [text exposition format]: https://prometheus.io/docs/instrumenting/exposition_formats/

use crate::{
    backend::{Backend, CallbackValue, Histogram, MetricInfo, MetricKind, ReportOptions},
    Unit,
};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
};

/// A [`Backend`] that accumulates cumulative Prometheus series and publishes the rendered exposition
/// text for a scrape endpoint to serve.
///
/// Construct with [`new`](Self::new), which also returns a [`PrometheusHandle`] the scrape endpoint
/// holds. The backend is long-lived and reused across reports: its cumulative state persists (that
/// is the whole point), so nothing is cleared in [`report_start`](Backend::report_start) beyond
/// per-report options.
pub struct PrometheusBackend {
    /// Authoritative cumulative state, keyed by the sanitized (display) metric name.
    families: BTreeMap<Arc<str>, Family>,
    /// Cache of raw metric name (the registry's shared `Arc<str>`) -> sanitized display name, so the
    /// sanitize+allocate conversion runs once per distinct name instead of every report. Populated
    /// freely (bounded by the fixed set of registered names); interning a name does not emit it.
    name_cache: BTreeMap<Arc<str>, Arc<str>>,
    /// Optional prefix prepended to every metric name (joined with `_`).
    prefix: Option<String>,
    /// Shared empty-aggregation sentinel, cloned (refcount bump) to build series keys for metrics
    /// with no aggregation, avoiding a fresh allocation per record.
    empty: Arc<str>,
    /// The rendered exposition text most recently published for scraping.
    published: Arc<Mutex<Arc<str>>>,
    include_sparse: bool,
}

/// A cloneable handle a scrape endpoint uses to read the most recently published exposition text.
///
/// Cheap to clone and to read: [`encode`](Self::encode) clones a shared `Arc<str>` under a
/// short-lived lock (no formatting on the scrape path — that happens once per report on the
/// reporting thread).
#[derive(Clone)]
pub struct PrometheusHandle {
    published: Arc<Mutex<Arc<str>>>,
}

impl PrometheusHandle {
    /// The exposition text published by the most recent report, as a shared `Arc<str>`.
    ///
    /// Returns an empty string until the first report completes.
    pub fn encode(&self) -> Arc<str> {
        self.published.lock().unwrap().clone()
    }

    /// The exposition text published by the most recent report, as an owned `String`.
    pub fn render(&self) -> String {
        self.encode().to_string()
    }
}

impl PrometheusBackend {
    /// Creates a backend and the [`PrometheusHandle`] a scrape endpoint reads from.
    ///
    /// `prefix`, when non-empty, is prepended to every metric name (joined with `_`).
    pub fn new(prefix: Option<String>) -> (Self, PrometheusHandle) {
        let published: Arc<Mutex<Arc<str>>> = Arc::new(Mutex::new(Arc::from("")));
        let handle = PrometheusHandle {
            published: published.clone(),
        };
        (
            Self {
                families: BTreeMap::new(),
                name_cache: BTreeMap::new(),
                prefix,
                empty: Arc::from(""),
                published,
                include_sparse: false,
            },
            handle,
        )
    }

    /// The sanitized, prefixed display name for `info`, as a shared `Arc<str>`.
    ///
    /// Cached on the registry's raw name handle: the sanitize+allocate work runs once per distinct
    /// metric name, and thereafter every report just clones an `Arc` (a refcount bump). The
    /// registry hands us the *same* `Arc<str>` for a given metric each report (see `MetricInfo`),
    /// so the cache key is stable and cheap to look up.
    fn display_name(&mut self, info: &MetricInfo<'_>) -> Arc<str> {
        if let Some(name) = self.name_cache.get(info.name) {
            return name.clone();
        }
        let mut sanitized = String::new();
        write_name(&mut sanitized, self.prefix.as_deref(), info.name);
        let sanitized: Arc<str> = Arc::from(sanitized);
        self.name_cache.insert(info.name.clone(), sanitized.clone());
        sanitized
    }

    /// Whether a series is already retained under the display `name`/`key`.
    ///
    /// Used to decide suppression *without* materializing the family: `Registry::report` visits
    /// every registered metric every interval (zeros included), so inserting on the suppressed path
    /// would grow `families` to hold every metric name forever, defeating zero-suppression.
    fn series_present(&self, name: &Arc<str>, key: &SeriesKey) -> bool {
        self.families
            .get(name)
            .is_some_and(|f| f.series.contains_key(key))
    }

    /// Looks up (or creates) the family for the given sanitized `name`.
    ///
    /// The unit and kind are taken from the first record for a given name; a name is assumed to have
    /// a single kind across all its aggregations (as every call site registers it), matching the
    /// per-family `# TYPE` requirement of the exposition format. Call only once the record has been
    /// decided to be retained, so a suppressed metric never inserts an empty family.
    fn family_or_insert(&mut self, name: Arc<str>, info: &MetricInfo<'_>) -> &mut Family {
        self.families.entry(name).or_insert_with(|| Family {
            kind: info.kind,
            unit: info.unit,
            series: BTreeMap::new(),
        })
    }

    /// The series key for `info` and an optional bool side.
    ///
    /// Shares the registry's aggregation `Arc<str>` (a refcount bump, not a fresh allocation);
    /// metrics with no aggregation reuse the shared [`empty`](Self::empty) sentinel.
    fn series_key(&self, info: &MetricInfo<'_>, side: Option<BoolSide>) -> SeriesKey {
        let aggregation = match info.aggregation {
            Some(agg) => agg.clone(),
            None => self.empty.clone(),
        };
        SeriesKey { aggregation, side }
    }

    /// Renders the full cumulative state into the Prometheus text exposition format.
    fn encode(&self) -> String {
        let mut out = String::new();
        for (name, family) in &self.families {
            if family.series.is_empty() {
                continue;
            }
            writeln!(out, "# TYPE {name} {}", family.prom_type()).unwrap();
            for entry in family.series.values() {
                // Labels were parsed once when the series was created; reuse them here.
                entry
                    .state
                    .encode(&mut out, name, &entry.labels, family.unit);
            }
        }
        out
    }

    /// Encodes the current state and publishes it for scraping.
    fn publish(&self) {
        let text = self.encode();
        *self.published.lock().unwrap() = Arc::from(text);
    }
}

impl Backend for PrometheusBackend {
    fn report_start(&mut self, options: &ReportOptions) {
        // The cumulative state is deliberately NOT cleared: Prometheus series persist across
        // reports. Only per-report options are refreshed.
        self.include_sparse = options.include_sparse;
    }

    fn record_counter(&mut self, info: &MetricInfo<'_>, value: u64) {
        // Skip a counter that has never been non-zero (avoids clutter). Once a series exists it is
        // retained and keeps rendering its cumulative total, even when a later delta is zero, so the
        // time series has no gaps.
        let name = self.display_name(info);
        let key = self.series_key(info, None);
        // Suppress a counter that has never been non-zero, without materializing its family (see
        // `series_present`): the registry re-visits every metric each interval, so inserting here
        // would leak an empty family per zero-only metric.
        if value == 0 && !info.emit_zero(self.include_sparse) && !self.series_present(&name, &key) {
            return;
        }
        let family = self.family_or_insert(name, info);
        match &mut family.entry(key).state {
            Series::Counter(total) => *total += value,
            _ => unreachable!("counter family has non-counter series"),
        }
    }

    fn record_gauge(&mut self, info: &MetricInfo<'_>, value: i64) {
        let name = self.display_name(info);
        let key = self.series_key(info, None);
        if value == 0 && !info.emit_zero(self.include_sparse) && !self.series_present(&name, &key) {
            return;
        }
        let family = self.family_or_insert(name, info);
        family.entry(key).state = Series::Gauge(value as f64);
    }

    fn record_bool(&mut self, info: &MetricInfo<'_>, true_count: u64, false_count: u64) {
        // One counter family with a `result` label per side; each side accumulates like a counter.
        let name = self.display_name(info);
        let emit_zero = info.emit_zero(self.include_sparse);
        for (side, delta) in [(BoolSide::True, true_count), (BoolSide::False, false_count)] {
            let key = self.series_key(info, Some(side));
            if delta == 0 && !emit_zero && !self.series_present(&name, &key) {
                continue;
            }
            let family = self.family_or_insert(name.clone(), info);
            match &mut family.entry(key).state {
                Series::Counter(total) => *total += delta,
                _ => unreachable!("bool family has non-counter series"),
            }
        }
    }

    fn record_histogram(&mut self, info: &MetricInfo<'_>, hist: Histogram<'_>) {
        let unit = info.unit;
        let name = self.display_name(info);
        let key = self.series_key(info, None);
        if hist.count() == 0
            && !info.emit_zero(self.include_sparse)
            && !self.series_present(&name, &key)
        {
            return;
        }
        let bounds = default_buckets(unit);
        let family = self.family_or_insert(name, info);
        let series = &mut family
            .entry_with(key, || Series::Histogram(HistState::new(bounds.len())))
            .state;
        let Series::Histogram(state) = series else {
            unreachable!("histogram family has non-histogram series");
        };
        // Fold this report's drained distribution into the cumulative per-bucket counts. Each
        // non-empty crate bucket contributes its `count` to the smallest `le` slot that covers its
        // representative value; the trailing slot is the `+Inf` overflow. The running sum uses the
        // reported (unit-converted, de-scaled) value, matching the statsd backend's magnitude choice.
        let scale = hist.scale();
        for (value, count) in hist.buckets() {
            let reported = to_reported(value, unit) / scale;
            let slot = bounds
                .iter()
                .position(|&le| reported <= le)
                .unwrap_or(bounds.len());
            state.counts[slot] += count;
            state.sum += reported * count as f64;
            state.count += count;
        }
    }

    fn record_callback(&mut self, info: &MetricInfo<'_>, values: &[&dyn CallbackValue]) {
        // Callbacks are gauge-like instantaneous readings. As in statsd, multiple values under one
        // name are summed (the registry discards per-element identity), so a single reduced value is
        // the only faithful representation.
        if values.is_empty() {
            return;
        }
        let sum: f64 = values.iter().map(|v| v.as_f64()).sum();
        let name = self.display_name(info);
        let key = self.series_key(info, None);
        if sum == 0.0 && !info.emit_zero(self.include_sparse) && !self.series_present(&name, &key) {
            return;
        }
        let family = self.family_or_insert(name, info);
        family.entry(key).state = Series::Gauge(sum);
    }

    fn report_end(&mut self) {
        self.publish();
    }
}

/// A metric family: all series sharing one name (and therefore one `# TYPE`).
struct Family {
    kind: MetricKind,
    unit: Unit,
    /// Series keyed by their [`SeriesKey`] (aggregation `Arc<str>` + optional bool side).
    series: BTreeMap<SeriesKey, SeriesEntry>,
}

impl Family {
    /// The Prometheus type string for this family's `# TYPE` line.
    fn prom_type(&self) -> &'static str {
        match self.kind {
            MetricKind::Counter | MetricKind::BoolCounter => "counter",
            MetricKind::Gauge | MetricKind::CallbackScalar => "gauge",
            MetricKind::Histogram => "histogram",
        }
    }

    /// Looks up (or creates, defaulting to a zeroed [`Series`] matching the family kind) the entry
    /// for `key`. The label set is parsed once, when the entry is first created.
    fn entry(&mut self, key: SeriesKey) -> &mut SeriesEntry {
        let kind = self.kind;
        self.entry_with(key, || Series::zeroed(kind))
    }

    /// Like [`entry`](Self::entry) but with an explicit initializer (used for histograms, whose
    /// zeroed state needs the bucket count).
    fn entry_with(&mut self, key: SeriesKey, init: impl FnOnce() -> Series) -> &mut SeriesEntry {
        self.series
            .entry(key)
            .or_insert_with_key(|key| SeriesEntry {
                labels: key.labels(),
                state: init(),
            })
    }
}

/// A series' cached labels plus its cumulative state.
struct SeriesEntry {
    /// Labels derived from the key's aggregation/side, parsed once at creation and reused every
    /// report (label parsing is otherwise pure overhead on the encode path).
    labels: Vec<(Cow<'static, str>, Arc<str>)>,
    state: Series,
}

/// Which side of a bool counter a series represents.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BoolSide {
    True,
    False,
}

impl BoolSide {
    fn as_str(self) -> &'static str {
        match self {
            BoolSide::True => "true",
            BoolSide::False => "false",
        }
    }
}

/// Identifies a series within a family: the raw aggregation (shared with the registry) plus, for
/// bool counters, which `result` side it is.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    aggregation: Arc<str>,
    side: Option<BoolSide>,
}

impl SeriesKey {
    /// Parses this key into ordered `(label_name, label_value)` pairs.
    ///
    /// Prepends the boolean `result` label when present, then labels each aggregation tag.
    /// Keyless tags use `aggregation`; keyed tags use sanitized label names.
    fn labels(&self) -> Vec<(Cow<'static, str>, Arc<str>)> {
        let mut labels = Vec::new();
        if let Some(side) = self.side {
            labels.push((Cow::Borrowed("result"), Arc::from(side.as_str())));
        }
        for tag in crate::AggregationTags::parse(&self.aggregation).iter() {
            if tag.key.is_empty() {
                let value = if tag.value == &*self.aggregation {
                    self.aggregation.clone()
                } else {
                    Arc::from(tag.value)
                };
                labels.push((Cow::Borrowed("aggregation"), value));
            } else {
                labels.push((
                    Cow::Owned(sanitize_label_name(tag.key)),
                    Arc::from(tag.value),
                ));
            }
        }
        labels
    }
}

/// The cumulative state of a single Prometheus series.
enum Series {
    /// Monotonic cumulative total.
    Counter(u64),
    /// Latest instantaneous value.
    Gauge(f64),
    /// Cumulative classic-histogram state.
    Histogram(HistState),
}

impl Series {
    /// A zeroed series matching the family kind. Histograms are created via `entry_with` instead
    /// (they need the bucket count), so this maps `Histogram` kinds to a zero-bucket placeholder
    /// that is never actually used for that kind.
    fn zeroed(kind: MetricKind) -> Self {
        match kind {
            MetricKind::Counter | MetricKind::BoolCounter => Series::Counter(0),
            MetricKind::Gauge | MetricKind::CallbackScalar => Series::Gauge(0.0),
            MetricKind::Histogram => Series::Histogram(HistState::new(0)),
        }
    }
}

/// Cumulative classic-histogram state: per-`le`-slot observation counts (the last slot is `+Inf`),
/// plus the running sum and total observation count.
struct HistState {
    /// `counts[i]` is the number of observations that fell in `le` slot `i`, accumulated across
    /// reports (non-cumulative; the exposition emits the running prefix sum). `len == bounds + 1`,
    /// the extra slot being `+Inf`.
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

impl HistState {
    fn new(num_bounds: usize) -> Self {
        Self {
            counts: vec![0; num_bounds + 1],
            sum: 0.0,
            count: 0,
        }
    }
}

impl Series {
    /// Appends this series' sample line(s) to `out`.
    ///
    /// Everything is written straight into the shared `out` buffer — no per-sample temporary
    /// strings for the `_bucket`/`_sum`/`_count` names, the `le` boundary, or float values.
    fn encode(
        &self,
        out: &mut String,
        name: &str,
        labels: &[(Cow<'static, str>, Arc<str>)],
        unit: Unit,
    ) {
        match self {
            Series::Counter(v) => {
                write_series_header(out, name, "", labels, None);
                writeln!(out, " {v}").unwrap();
            }
            Series::Gauge(v) => {
                write_series_header(out, name, "", labels, None);
                out.push(' ');
                write_f64(out, *v);
                out.push('\n');
            }
            Series::Histogram(state) => {
                let bounds = default_buckets(unit);
                // Cumulative bucket counts: each `le` bucket is the count of observations <= le.
                let mut cumulative = 0u64;
                for (i, &le) in bounds.iter().enumerate() {
                    cumulative += state.counts[i];
                    write_series_header(out, name, "_bucket", labels, Some(Le::Value(le)));
                    writeln!(out, " {cumulative}").unwrap();
                }
                // The `+Inf` bucket always equals the total observation count.
                write_series_header(out, name, "_bucket", labels, Some(Le::Inf));
                writeln!(out, " {}", state.count).unwrap();
                write_series_header(out, name, "_sum", labels, None);
                out.push(' ');
                write_f64(out, state.sum);
                out.push('\n');
                write_series_header(out, name, "_count", labels, None);
                writeln!(out, " {}", state.count).unwrap();
            }
        }
    }
}

/// The `le` label on a histogram bucket sample: either a boundary value or the `+Inf` overflow.
enum Le {
    Value(f64),
    Inf,
}

/// Writes `name{suffix}{labels...,le="..."}` (no trailing space or value) into `out`.
///
/// `name` and `suffix` (`""`, `_bucket`, `_sum`, `_count`) are written directly; `base` are the
/// series' cached aggregation/result labels; `le`, when present, appends the histogram `le` label
/// last. Everything goes straight into `out` — no intermediate allocations. When there are no
/// labels at all, no `{}` is written.
fn write_series_header(
    out: &mut String,
    name: &str,
    suffix: &str,
    base: &[(Cow<'static, str>, Arc<str>)],
    le: Option<Le>,
) {
    out.push_str(name);
    out.push_str(suffix);
    if base.is_empty() && le.is_none() {
        return;
    }
    out.push('{');
    let mut first = true;
    for (k, v) in base {
        write_label(out, &mut first, k, v);
    }
    if let Some(le) = le {
        if !first {
            out.push(',');
        }
        out.push_str("le=\"");
        match le {
            Le::Value(v) => write_f64(out, v),
            Le::Inf => out.push_str("+Inf"),
        }
        out.push('"');
    }
    out.push('}');
}

/// Writes a single `key="value"` label, prefixing a `,` when it is not the first.
fn write_label(out: &mut String, first: &mut bool, key: &str, value: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    out.push_str(key);
    out.push_str("=\"");
    write_escaped_value(out, value);
    out.push('"');
}

/// Default `le` bucket boundaries for a unit, in the reported (converted) magnitude.
///
/// The crate histogram has thousands of fine buckets, far too many to emit per scrape; the reporter
/// collapses them onto this bounded, per-unit boundary set. Time units report **seconds** (values
/// are stored as nanoseconds), bytes and counts report their raw magnitude, and percent reports a
/// whole percentage.
fn default_buckets(unit: Unit) -> &'static [f64] {
    match unit {
        Unit::Microsecond | Unit::Second => &[
            0.000_001, 0.000_01, 0.000_1, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0,
            2.5, 5.0, 10.0, 30.0, 60.0,
        ],
        Unit::Byte => &[
            64.0,
            256.0,
            1024.0,
            4096.0,
            16384.0,
            65536.0,
            262_144.0,
            1_048_576.0,
            4_194_304.0,
            16_777_216.0,
            67_108_864.0,
            268_435_456.0,
            1_073_741_824.0,
        ],
        Unit::Count => &[
            1.0,
            2.0,
            5.0,
            10.0,
            25.0,
            50.0,
            100.0,
            250.0,
            500.0,
            1000.0,
            5000.0,
            10000.0,
            50000.0,
            100_000.0,
            1_000_000.0,
        ],
        Unit::Percent => &[1.0, 5.0, 10.0, 25.0, 50.0, 75.0, 90.0, 95.0, 99.0, 100.0],
    }
}

/// Converts a native histogram bucket value to its reported Prometheus magnitude, before the
/// histogram's fixed-point [`scale`](crate::Summary::scale) is divided out by the caller.
///
/// Mirrors the storage conventions the statsd backend documents: time units store nanoseconds (from
/// `record_duration`) and are reported as seconds; `Count`/`Byte`/`Percent` are raw. A fractional
/// (scaled) histogram has its scale divided out by the caller after this conversion.
fn to_reported(value: u64, unit: Unit) -> f64 {
    match unit {
        Unit::Microsecond | Unit::Second => value as f64 / 1_000_000_000.0,
        Unit::Count | Unit::Byte | Unit::Percent => value as f64,
    }
}

/// Writes an `f64` to `out` in the exposition format: integers render without a decimal point, and
/// non-finite values map to the Prometheus spellings (`+Inf`, `-Inf`, `NaN`). Writes directly into
/// the shared output buffer — no intermediate `String`.
fn write_f64(out: &mut String, v: f64) {
    if v.is_nan() {
        out.push_str("NaN");
    } else if v.is_infinite() {
        out.push_str(if v > 0.0 { "+Inf" } else { "-Inf" });
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        write!(out, "{}", v as i64).unwrap();
    } else {
        write!(out, "{v}").unwrap();
    }
}

/// Appends the sanitized, prefixed metric name to `out`.
///
/// Sanitization keeps `[a-zA-Z0-9_:]` and maps everything else to `_`, matching the Prometheus
/// metric-name grammar. A prefix, when non-empty, is joined to the name with `_`. The Prometheus
/// grammar additionally forbids a leading digit (`[a-zA-Z_:][a-zA-Z0-9_:]*`), so a name that would
/// begin with a digit is prefixed with `_`.
fn write_name(out: &mut String, prefix: Option<&str>, name: &str) {
    let start = out.len();
    if let Some(prefix) = prefix.filter(|p| !p.is_empty()) {
        write_sanitized_name(out, prefix);
        if out.len() != start {
            out.push('_');
        }
    }
    write_sanitized_name(out, name);
    // A metric name must not start with a digit; guard the first character we wrote.
    if out[start..].starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(start, '_');
    }
}

/// Appends `input` to `out`, sanitized for a Prometheus metric name.
fn write_sanitized_name(out: &mut String, input: &str) {
    out.reserve(input.len());
    for c in input.chars() {
        out.push(match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | ':' => c,
            _ => '_',
        });
    }
}

/// Sanitizes a label name to the Prometheus grammar (`[a-zA-Z_][a-zA-Z0-9_]*`), lowercasing ASCII
/// letters so `Variant` -> `variant`.
fn sanitize_label_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        out.push(match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '_' => c,
            _ => '_',
        });
    }
    // An empty input (e.g. an aggregation like `|value`) would emit an invalid `="value"` label;
    // fall back to a generic name. A label also must not start with a digit.
    if out.is_empty() {
        return "aggregation".to_string();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Appends `value` to `out`, escaping `\`, `"`, and newlines as the exposition format requires.
fn write_escaped_value(out: &mut String, value: &str) {
    out.reserve(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Builds an `Arc<str>` for a test metric name/aggregation. The returned value lives to the end
    /// of the enclosing statement, which is long enough for the borrow taken by [`info`].
    fn arc(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    fn info<'a>(
        name: &'a Arc<str>,
        agg: Option<&'a Arc<str>>,
        unit: Unit,
        kind: MetricKind,
    ) -> MetricInfo<'a> {
        MetricInfo::new(name, agg, unit, kind)
    }

    /// Collects the rendered lines for easy assertions.
    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn counter_accumulates_across_reports() {
        let (mut backend, handle) = PrometheusBackend::new(Some("svc".into()));

        // First report: delta 7.
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(&arc("rx.data"), None, Unit::Count, MetricKind::Counter),
            7,
        );
        backend.report_end();
        assert!(lines(&handle.render()).contains(&"svc_rx_data 7"));

        // Second report: delta 5 -> cumulative 12 (proves accumulation, not last-write).
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(&arc("rx.data"), None, Unit::Count, MetricKind::Counter),
            5,
        );
        backend.report_end();
        let rendered = handle.render();
        assert!(lines(&rendered).contains(&"svc_rx_data 12"), "{rendered}");
        // TYPE line present exactly once.
        assert_eq!(
            rendered.matches("# TYPE svc_rx_data counter").count(),
            1,
            "{rendered}"
        );
    }

    #[test]
    fn zero_counter_delta_retains_existing_series() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_counter(&info(&arc("c"), None, Unit::Count, MetricKind::Counter), 4);
        backend.report_end();

        // A zero delta must not drop the series; the cumulative total stays and keeps rendering.
        backend.report_start(&ReportOptions::default());
        backend.record_counter(&info(&arc("c"), None, Unit::Count, MetricKind::Counter), 0);
        backend.report_end();
        assert!(lines(&handle.render()).contains(&"c 4"));
    }

    #[test]
    fn never_nonzero_counter_is_suppressed_unless_sparse() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::new(false));
        backend.record_counter(&info(&arc("c"), None, Unit::Count, MetricKind::Counter), 0);
        backend.report_end();
        assert_eq!(handle.render(), "");

        // Sparse keeps it.
        backend.report_start(&ReportOptions::new(true));
        backend.record_counter(&info(&arc("c"), None, Unit::Count, MetricKind::Counter), 0);
        backend.report_end();
        assert!(lines(&handle.render()).contains(&"c 0"));
    }

    #[test]
    fn aggregation_becomes_a_label() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(
                &arc("rx.ecn"),
                Some(&arc("Variant|ect0")),
                Unit::Count,
                MetricKind::Counter,
            ),
            9,
        );
        backend.record_counter(
            &info(
                &arc("rx.ecn"),
                Some(&arc("Variant|ce")),
                Unit::Count,
                MetricKind::Counter,
            ),
            3,
        );
        backend.report_end();
        let rendered = handle.render();
        assert!(
            lines(&rendered).contains(&"rx_ecn{variant=\"ect0\"} 9"),
            "{rendered}"
        );
        assert!(
            lines(&rendered).contains(&"rx_ecn{variant=\"ce\"} 3"),
            "{rendered}"
        );
        // Both series share one TYPE line.
        assert_eq!(rendered.matches("# TYPE rx_ecn counter").count(), 1);
    }

    #[test]
    fn multi_tag_aggregation_becomes_one_label_per_tag() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(
                &arc("api.calls"),
                Some(&arc("operation|get,result|ok")),
                Unit::Count,
                MetricKind::Counter,
            ),
            5,
        );
        backend.report_end();
        let rendered = handle.render();
        assert!(
            lines(&rendered).contains(&"api_calls{operation=\"get\",result=\"ok\"} 5"),
            "{rendered}"
        );
    }

    #[test]
    fn bare_aggregation_uses_generic_label() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(
                &arc("m"),
                Some(&arc("misc")),
                Unit::Count,
                MetricKind::Counter,
            ),
            1,
        );
        backend.report_end();
        assert!(lines(&handle.render()).contains(&"m{aggregation=\"misc\"} 1"));
    }

    #[test]
    fn gauge_is_last_write_wins() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_gauge(
            &info(&arc("q.depth"), None, Unit::Count, MetricKind::Gauge),
            7,
        );
        backend.report_end();
        assert!(lines(&handle.render()).contains(&"q_depth 7"));

        // A later report overwrites (does not accumulate).
        backend.report_start(&ReportOptions::default());
        backend.record_gauge(
            &info(&arc("q.depth"), None, Unit::Count, MetricKind::Gauge),
            3,
        );
        backend.report_end();
        let rendered = handle.render();
        assert!(lines(&rendered).contains(&"q_depth 3"), "{rendered}");
        assert!(!lines(&rendered).contains(&"q_depth 10"));
    }

    #[test]
    fn bool_counter_uses_result_label_and_accumulates() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_bool(
            &info(&arc("connect"), None, Unit::Count, MetricKind::BoolCounter),
            2,
            1,
        );
        backend.report_end();

        backend.report_start(&ReportOptions::default());
        backend.record_bool(
            &info(&arc("connect"), None, Unit::Count, MetricKind::BoolCounter),
            3,
            0,
        );
        backend.report_end();

        let rendered = handle.render();
        assert!(
            lines(&rendered).contains(&"connect{result=\"true\"} 5"),
            "{rendered}"
        );
        assert!(
            lines(&rendered).contains(&"connect{result=\"false\"} 1"),
            "{rendered}"
        );
        assert_eq!(rendered.matches("# TYPE connect counter").count(), 1);
    }

    #[test]
    fn callback_sums_and_is_gauge() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        let a: u64 = 3;
        let b: u64 = 4;
        let values: [&dyn CallbackValue; 2] = [&a, &b];
        backend.record_callback(
            &info(
                &arc("workers"),
                None,
                Unit::Count,
                MetricKind::CallbackScalar,
            ),
            &values,
        );
        backend.report_end();
        let rendered = handle.render();
        assert!(rendered.contains("# TYPE workers gauge"), "{rendered}");
        assert!(lines(&rendered).contains(&"workers 7"), "{rendered}");
    }

    #[test]
    fn histogram_emits_cumulative_buckets_sum_and_count() {
        // Build a real Summary so bucketing is genuine. record_duration stores nanoseconds; the
        // backend reports seconds, so 5us -> 0.000005s lands in the 0.00001 le bucket.
        let registry = crate::Registry::new();
        let summary = registry.register_summary("task.time".into(), None, Unit::Microsecond);
        summary.record_duration(std::time::Duration::from_micros(5));
        summary.record_duration(std::time::Duration::from_micros(5));
        summary.record_duration(std::time::Duration::from_micros(10));

        let (mut backend, handle) = PrometheusBackend::new(None);
        registry.report(&mut backend);
        let rendered = handle.render();

        assert!(
            rendered.contains("# TYPE task_time histogram"),
            "{rendered}"
        );
        // Three observations total.
        assert!(
            lines(&rendered).contains(&"task_time_count 3"),
            "{rendered}"
        );
        // +Inf bucket equals the count.
        assert!(
            lines(&rendered).contains(&"task_time_bucket{le=\"+Inf\"} 3"),
            "{rendered}"
        );
        // Buckets are cumulative and monotonic: the highest bucket reaches 3, and the smallest that
        // any sample falls into is >= the number of small samples.
        let bucket_lines: Vec<&str> = rendered
            .lines()
            .filter(|l| l.starts_with("task_time_bucket"))
            .collect();
        let values: Vec<u64> = bucket_lines
            .iter()
            .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
            .collect();
        assert!(
            values.windows(2).all(|w| w[0] <= w[1]),
            "buckets must be non-decreasing: {values:?}"
        );
    }

    #[test]
    fn histogram_accumulates_across_reports() {
        let registry = crate::Registry::new();
        let summary = registry.register_summary("lat".into(), None, Unit::Count);
        summary.record_value(5);

        let (mut backend, handle) = PrometheusBackend::new(None);
        registry.report(&mut backend);
        assert!(lines(&handle.render()).contains(&"lat_count 1"));

        // A second interval adds another observation -> cumulative count 2.
        summary.record_value(5);
        registry.report(&mut backend);
        assert!(lines(&handle.render()).contains(&"lat_count 2"));
    }

    #[test]
    fn label_values_are_escaped() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(
                &arc("m"),
                Some(&arc("Variant|a\"b\\c")),
                Unit::Count,
                MetricKind::Counter,
            ),
            1,
        );
        backend.report_end();
        assert!(
            handle.render().contains("m{variant=\"a\\\"b\\\\c\"} 1"),
            "{}",
            handle.render()
        );
    }

    #[test]
    fn empty_report_publishes_empty_string() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.report_end();
        assert_eq!(handle.render(), "");
    }

    #[test]
    fn handle_reflects_latest_publish() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        // Before any report the handle is empty.
        assert_eq!(handle.render(), "");
        backend.report_start(&ReportOptions::default());
        backend.record_counter(&info(&arc("c"), None, Unit::Count, MetricKind::Counter), 1);
        backend.report_end();
        assert!(!handle.render().is_empty());
    }

    /// A suppressed (never-non-zero) metric must not leave an empty family behind: the registry
    /// re-visits every metric each interval, so a leaked family per zero-only metric would grow
    /// unbounded and defeat zero-suppression.
    #[test]
    fn suppressed_metric_does_not_retain_family() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        // Many intervals of an always-zero counter under the normal (non-sparse) policy.
        for _ in 0..5 {
            backend.report_start(&ReportOptions::new(false));
            backend.record_counter(&info(&arc("z"), None, Unit::Count, MetricKind::Counter), 0);
            backend.record_gauge(&info(&arc("g"), None, Unit::Count, MetricKind::Gauge), 0);
            backend.record_bool(
                &info(&arc("b"), None, Unit::Count, MetricKind::BoolCounter),
                0,
                0,
            );
            backend.report_end();
        }
        // Nothing retained, nothing rendered.
        assert!(
            backend.families.is_empty(),
            "leaked families: {:?}",
            backend.families.keys().collect::<Vec<_>>()
        );
        assert_eq!(handle.render(), "");
    }

    /// A metric name (or prefix) that would begin with a digit must be prefixed with `_` so the
    /// exposition identifier is valid.
    #[test]
    fn leading_digit_name_is_escaped() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(&arc("5xx"), None, Unit::Count, MetricKind::Counter),
            1,
        );
        backend.report_end();
        let rendered = handle.render();
        assert!(rendered.contains("# TYPE _5xx counter"), "{rendered}");
        assert!(lines(&rendered).contains(&"_5xx 1"), "{rendered}");

        // A digit-leading prefix is likewise guarded.
        let (mut backend, handle) = PrometheusBackend::new(Some("9svc".into()));
        backend.report_start(&ReportOptions::default());
        backend.record_counter(&info(&arc("ok"), None, Unit::Count, MetricKind::Counter), 1);
        backend.report_end();
        assert!(
            lines(&handle.render()).contains(&"_9svc_ok 1"),
            "{}",
            handle.render()
        );
    }

    /// An aggregation whose label-name half is empty (e.g. `|value`) must not emit an invalid
    /// `="value"` label; it falls back to the generic `aggregation` name.
    #[test]
    fn empty_label_name_falls_back_to_generic() {
        let (mut backend, handle) = PrometheusBackend::new(None);
        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(
                &arc("m"),
                Some(&arc("|value")),
                Unit::Count,
                MetricKind::Counter,
            ),
            1,
        );
        backend.report_end();
        assert!(
            lines(&handle.render()).contains(&"m{aggregation=\"value\"} 1"),
            "{}",
            handle.render()
        );
    }

    /// The display-name conversion is memoized on the registry's shared name handle: the same raw
    /// `Arc<str>` reported across many intervals sanitizes once, and later reports clone the cached
    /// `Arc` rather than re-sanitizing. Proven by the cache holding a single entry and the cached
    /// value pointer-identical to what a repeated `display_name` returns.
    #[test]
    fn display_name_is_cached_across_reports() {
        let (mut backend, _handle) = PrometheusBackend::new(Some("svc".into()));
        // The registry hands the *same* Arc<str> for a metric each report; model that with one arc.
        let name = arc("rx.data");

        for _ in 0..3 {
            backend.report_start(&ReportOptions::default());
            backend.record_counter(&info(&name, None, Unit::Count, MetricKind::Counter), 1);
            backend.report_end();
        }

        // Only one cache entry despite three reports, and it is keyed on the raw name.
        assert_eq!(backend.name_cache.len(), 1);
        let cached = backend.name_cache.get(&name).expect("name cached").clone();
        assert_eq!(&*cached, "svc_rx_data");

        // A subsequent lookup returns the *same allocation* (pointer identity), i.e. a refcount
        // bump, not a fresh sanitize+alloc.
        let again = backend.display_name(&info(&name, None, Unit::Count, MetricKind::Counter));
        assert!(Arc::ptr_eq(&cached, &again), "display_name re-allocated");
    }
}
