// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! A [`Backend`] that encodes metrics into the StatsD line protocol from native values.
//!
//! This is the formatting + batching half of StatsD export. It is transport-agnostic: finished
//! UDP payloads are handed to a [`StatsdSink`], which the consumer implements (e.g. an
//! `s2n-quic-dc` rate-paced UDP socket). The crate intentionally does not depend on any socket or
//! pacing machinery.
//!
//! Dispatch is purely on the generic [`MetricInfo`] (`kind` + `unit`); `name` and `aggregation`
//! are treated as opaque. No metric-name conventions are interpreted here.

use crate::{
    backend::{Backend, CallbackValue, Histogram, MetricInfo, ReportOptions},
    Unit,
};
use std::{fmt, fmt::Write as _, sync::Arc};

/// Default maximum UDP payload size (bytes) for a single datagram.
pub const DEFAULT_MAX_PAYLOAD_SIZE: usize = 1200;

/// A transport for finished StatsD UDP datagrams.
///
/// The backend formats and chunks records into datagram-sized payloads and hands them off here in
/// `report_end`, as an iterator of `&str` slices that borrow the backend's internal buffer.
/// Implementations send them however they like (e.g. a rate-paced UDP socket); each `&str` is one
/// ready-to-send datagram. The slices are only valid for the duration of the call, so an
/// implementation that defers sending (e.g. queues to another task) must copy. `send_batch` should
/// not block the reporting thread; drop or queue as appropriate.
pub trait StatsdSink: Send {
    /// Sends a batch of datagrams. Each item is one complete, newline-joined UDP payload.
    fn send_batch<'a>(&mut self, payloads: impl Iterator<Item = &'a str>);
}

/// A [`Backend`] that encodes metrics as StatsD lines and forwards datagram payloads to a
/// [`StatsdSink`].
///
/// Reusable across reports: per-report state is cleared in
/// [`report_start`](Backend::report_start) while capacity is retained. All records for a report are
/// written into a single `buffer`, separated by `\n`; `bounds` holds each record's end offset so
/// chunking can yield datagrams as contiguous slices of `buffer` without copying.
pub struct StatsdBackend<S> {
    sink: S,
    prefix: Option<String>,
    max_payload_size: usize,
    include_sparse: bool,
    /// All records for the in-progress report, `\n`-separated.
    buffer: String,
    /// End offset (exclusive) of each record in `buffer`. Record `i` spans
    /// `[bounds[i-1] + 1, bounds[i])` (or `[0, bounds[0])` for `i == 0`); the byte at each
    /// `bounds[i-1]` is the `\n` separator.
    bounds: Vec<usize>,
}

impl<S> StatsdBackend<S> {
    /// Creates a backend that forwards to `sink`, optionally prefixing every metric name, with the
    /// default maximum payload size.
    pub fn new(sink: S, prefix: Option<String>) -> Self {
        Self {
            sink,
            prefix,
            max_payload_size: DEFAULT_MAX_PAYLOAD_SIZE,
            include_sparse: false,
            buffer: String::new(),
            bounds: Vec::new(),
        }
    }

    /// Sets the maximum UDP payload size used when chunking records into datagrams.
    pub fn with_max_payload_size(mut self, max_payload_size: usize) -> Self {
        self.max_payload_size = max_payload_size;
        self
    }

    /// Starts a new record: writes the `\n` separator (after the first) and the sanitized,
    /// prefixed metric name plus `name_suffix`, returning the buffer for the caller to append the
    /// value into. The matching [`end_record`](Self::end_record) records the boundary.
    ///
    /// Free-function-style field access (`&mut self.buffer` alongside `&self.prefix`) keeps the
    /// borrows disjoint so we never allocate an intermediate name string.
    fn begin_record(&mut self, name: &str, name_suffix: &str) -> &mut String {
        if !self.buffer.is_empty() {
            self.buffer.push('\n');
        }
        write_name(&mut self.buffer, self.prefix.as_deref(), name);
        self.buffer.push_str(name_suffix);
        &mut self.buffer
    }

    /// Finishes the current record: appends the aggregation's tags (see [`write_tag`]) and records
    /// the boundary.
    fn end_record(&mut self, aggregation: Option<&Arc<str>>) {
        write_tag(&mut self.buffer, aggregation.map(|a| a.as_ref()));
        self.bounds.push(self.buffer.len());
    }
}

impl<S: StatsdSink> Backend for StatsdBackend<S> {
    fn report_start(&mut self, options: &ReportOptions) {
        // Clear retains the allocated capacity (the backend is long-lived).
        self.buffer.clear();
        self.bounds.clear();
        self.include_sparse = options.include_sparse;
    }

    fn record_counter(&mut self, info: &MetricInfo<'_>, value: u64) {
        // Suppress zeros unless the metric's sparse policy (or the report) opts into emitting them,
        // so steady-state reporting isn't flooded with `:0|c` lines (matching the querylog policy).
        if value == 0 && !info.emit_zero(self.include_sparse) {
            return;
        }
        write!(self.begin_record(info.name, ":"), "{value}|c").unwrap();
        self.end_record(info.aggregation);
    }

    fn record_gauge(&mut self, info: &MetricInfo<'_>, value: i64) {
        // Honor the metric's sparse policy: drop a zero unless the metric (or report) opts in.
        if value == 0 && !info.emit_zero(self.include_sparse) {
            return;
        }
        write!(self.begin_record(info.name, ":"), "{value}|g").unwrap();
        self.end_record(info.aggregation);
    }

    fn record_bool(&mut self, info: &MetricInfo<'_>, true_count: u64, false_count: u64) {
        // Each side is its own counter, so apply the counter zero-suppression policy per side:
        // a zero count is omitted unless the metric's sparse policy opts in. (When both are zero
        // this emits nothing under the normal policy, matching the all-zero suppression elsewhere.)
        let emit_zero = info.emit_zero(self.include_sparse);
        if true_count != 0 || emit_zero {
            write!(self.begin_record(info.name, ".true:"), "{true_count}|c").unwrap();
            self.end_record(info.aggregation);
        }
        if false_count != 0 || emit_zero {
            write!(self.begin_record(info.name, ".false:"), "{false_count}|c").unwrap();
            self.end_record(info.aggregation);
        }
    }

    fn record_histogram(&mut self, info: &MetricInfo<'_>, hist: Histogram<'_>) {
        if hist.count() == 0 {
            return;
        }
        let unit = hist.unit();

        // Emit the raw distribution as DogStatsD histogram (`|h`) samples under the single base
        // metric name, one line per non-empty bucket. Each line carries a sample-rate weight of
        // `1/count`, so the bucket's representative value stands in for `count` observations. The
        // StatsD agent aggregates these into the base metric and computes percentiles server-side,
        // which lets per-host distributions roll up correctly across a fleet. Pre-computing
        // percentiles client-side and emitting them as separate `.count/.min/.pNN/.max` gauges
        // (the earlier encoding) produced distinct metric names — breaking dashboards that query
        // the base name with a server-side percentile statistic — and yielded per-host percentiles
        // that cannot be aggregated across hosts.
        //
        // Bucket values are the native recorded magnitude: nanoseconds for time units
        // (record_duration stores `as_nanos`), bytes/counts as-is. For an unscaled histogram
        // `statsd_value` maps that to the reported integer; a fractional histogram stores `scale * v`
        // and is de-scaled inline below (see the `scale != 1.0` branch) to emit a float sample.
        let scale = hist.scale();
        for (value, count) in hist.buckets() {
            // `buckets()` only yields non-empty buckets, so `count >= 1`; guard anyway so the
            // sample-rate division stays self-contained and can never produce `@inf` if that
            // contract ever changes.
            if count == 0 {
                continue;
            }
            // Sample-rate weight: this single sample represents `count` observations.
            let weight = 1.0 / count as f64;
            let out = self.begin_record(info.name, ":");
            // Unscaled: emit the exact integer magnitude (byte-identical to the historical encoding,
            // and free of f64 precision loss on large nanosecond midpoints). Scaled: emit the
            // de-scaled fractional value.
            if scale == 1.0 {
                let v = statsd_value(value, unit);
                write!(out, "{v}|h|@{}", Trimmed(weight)).unwrap();
            } else {
                let v = value as f64 / scale;
                write!(out, "{}|h|@{}", Trimmed(v), Trimmed(weight)).unwrap();
            }
            self.end_record(info.aggregation);
        }
    }

    fn record_callback(&mut self, info: &MetricInfo<'_>, values: &[&dyn CallbackValue]) {
        // Callback metrics are gauge-like instantaneous readings, emitted as a single gauge.
        //
        // When multiple callbacks share one name (e.g. one per worker) we emit their SUM. The
        // callback list carries no per-element identity (the registry discards which worker each
        // value came from), so a single reduced value is the only faithful StatsD representation.
        // Sum is correct for additive readings (counts, queue depths) but not for intensive ones
        // (e.g. per-worker busy-percent, where a mean would be more meaningful). Lifting these to a
        // first-class, per-variant metric kind — which would let StatsD emit one tagged line per
        // element — is deferred to a later phase; for now sum is the documented behavior.
        if values.is_empty() {
            return;
        }
        let sum: f64 = values.iter().map(|v| v.as_f64()).sum();
        // Honor the metric's sparse policy, as for gauges.
        if sum == 0.0 && !info.emit_zero(self.include_sparse) {
            return;
        }
        write!(self.begin_record(info.name, ":"), "{}|g", Trimmed(sum)).unwrap();
        self.end_record(info.aggregation);
    }

    fn report_end(&mut self) {
        if self.bounds.is_empty() {
            return;
        }
        // Chunk the records (delimited by `bounds`) into datagram-sized `&str` slices of `buffer`,
        // and hand them to the sink without copying.
        let chunks = Chunks::new(&self.buffer, &self.bounds, self.max_payload_size);
        self.sink.send_batch(chunks);
    }
}

/// Formats an `f64` with at most 6 decimal places, trimming trailing zeros but always
/// emitting at least one decimal place (e.g. `7.0`).
///
/// This bounds the output to at most 6 decimal places (e.g. `0.333333` for `1/3`) while
/// ensuring the value is always formatted as a float (e.g. `7.0` rather than `7`). The
/// implementation is allocation-free: finite values are rounded into a fixed-point integer,
/// trailing fractional zeros are stripped by integer division, and the result is emitted with a
/// width-controlled format specifier.
struct Trimmed(f64);

impl fmt::Display for Trimmed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.0.is_finite() {
            return write!(f, "{}", self.0);
        }

        const SCALE: u64 = 1_000_000; // 10^6 → 6 decimal places max
        let sign = if self.0.is_sign_negative() { "-" } else { "" };
        let scaled = (self.0.abs() * SCALE as f64).round() as u64;
        let int_part = scaled / SCALE;
        let mut frac = scaled % SCALE;
        let mut prec = 6usize;
        while prec > 0 && frac.is_multiple_of(10) {
            frac /= 10;
            prec -= 1;
        }
        if prec == 0 {
            write!(f, "{sign}{int_part}.0")
        } else {
            write!(f, "{sign}{int_part}.{frac:0>width$}", width = prec)
        }
    }
}

/// Maps a native (unscaled) histogram bucket value to the integer this backend reports.
///
/// Values are emitted as gauges in their native recorded magnitude:
/// - time units (`Microsecond`/`Second`) are stored as **nanoseconds** (`record_duration` records
///   `Duration::as_nanos`), and we report that nanosecond magnitude as-is;
/// - `Count`/`Byte`/`Percent` are raw and pass through.
///
/// Fractional histograms (scale != 1.0) don't route through here — the caller divides the scale out
/// and emits a float directly.
fn statsd_value(value: u64, unit: Unit) -> u64 {
    match unit {
        Unit::Microsecond | Unit::Second | Unit::Count | Unit::Byte | Unit::Percent => value,
    }
}

/// Appends the sanitized, prefixed metric name to `out`.
///
/// Sanitization: `[a-zA-Z0-9_.-]` is preserved, `:` becomes `.`, everything else becomes `_`.
fn write_name(out: &mut String, prefix: Option<&str>, name: &str) {
    if let Some(prefix) = prefix.filter(|p| !p.is_empty()) {
        let before = out.len();
        write_sanitized(out, prefix);
        // Only add the separating `.` if the prefix produced something.
        if out.len() != before {
            out.push('.');
        }
    }
    write_sanitized(out, name);
}

/// Appends sanitized aggregation tags; absent or empty aggregations emit nothing.
/// A single legacy tag uses `variant:<whole aggregation>`; other tag sets use per-tag keys and values.
fn write_tag(out: &mut String, aggregation: Option<&str>) {
    let Some(agg) = aggregation.filter(|a| !a.is_empty()) else {
        return;
    };

    let tags = crate::AggregationTags::parse(agg);
    let mut iter = tags.iter();
    let is_legacy = match (iter.next(), iter.next()) {
        (Some(tag), None) => crate::tags::is_legacy_tag_key(tag.key),
        _ => false,
    };

    if is_legacy {
        out.push_str("|#variant:");
        write_sanitized(out, agg);
        return;
    }

    for (i, tag) in tags.iter().enumerate() {
        out.push_str(if i == 0 { "|#" } else { "," });
        write_sanitized(
            out,
            if tag.key.is_empty() {
                "variant"
            } else {
                tag.key
            },
        );
        out.push(':');
        write_sanitized(out, tag.value);
    }
}

/// Appends `input` to `out`, sanitized for StatsD (see [`write_name`]).
fn write_sanitized(out: &mut String, input: &str) {
    out.reserve(input.len());
    for c in input.chars() {
        out.push(match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' | '-' => c,
            ':' => '.',
            _ => '_',
        });
    }
}

/// The statsd-safe form of `input`: `[A-Za-z0-9_.-]` kept, `:` to `.`, anything else to `_`.
pub fn sanitize_tag(input: &str) -> String {
    let mut out = String::new();
    write_sanitized(&mut out, input);
    out
}

/// Iterator over datagram-sized `&str` slices of a record buffer.
///
/// Records are already `\n`-separated in `buffer` and delimited by `bounds` (each entry is a
/// record's end offset). A datagram is a maximal run of whole records whose byte length fits in
/// `max_payload_size`; because records are contiguous and newline-joined in `buffer`, each datagram
/// is just a sub-slice of `buffer` — no copying. Records longer than `max_payload_size` (which
/// cannot fit a datagram) are skipped.
struct Chunks<'a> {
    buffer: &'a str,
    bounds: &'a [usize],
    max_payload_size: usize,
    /// Index into `bounds` of the next record to consider.
    next: usize,
}

impl<'a> Chunks<'a> {
    fn new(buffer: &'a str, bounds: &'a [usize], max_payload_size: usize) -> Self {
        Self {
            buffer,
            bounds,
            max_payload_size,
            next: 0,
        }
    }

    /// The byte range `[start, end)` of record `i` in `buffer` (excluding its leading separator).
    fn record_range(&self, i: usize) -> (usize, usize) {
        // Record 0 starts at 0; record i (>0) starts one byte after the previous record's end
        // (skipping the `\n` separator).
        let start = if i == 0 { 0 } else { self.bounds[i - 1] + 1 };
        (start, self.bounds[i])
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.max_payload_size == 0 {
            return None;
        }

        // Skip records that can't fit a datagram on their own. This should not happen in practice:
        // records are short (`prefix.name.suffix:value|c|#variant:agg`) against a ~1200 byte
        // default, so an oversized record means a misconfiguration (tiny payload size or a
        // pathologically long metric name). Such records are silently dropped — like the previous
        // implementation, which also dropped them (it surfaced a dropped-count the reporter logged;
        // that diagnostic is not reproduced here to keep this crate free of a logging dependency).
        while self.next < self.bounds.len() {
            let (start, end) = self.record_range(self.next);
            if end - start <= self.max_payload_size {
                break;
            }
            self.next += 1;
        }
        if self.next >= self.bounds.len() {
            return None;
        }

        // Greedily extend the datagram with whole records (joined by their existing `\n`) while it
        // fits. `datagram_start` is the first record's start; the datagram spans to the last
        // included record's end.
        let (datagram_start, mut datagram_end) = self.record_range(self.next);
        self.next += 1;
        while self.next < self.bounds.len() {
            let (start, end) = self.record_range(self.next);
            if end - start > self.max_payload_size {
                // This record can't fit anywhere; let the next iteration's skip-loop drop it.
                break;
            }
            // Including it means spanning from datagram_start..end, which absorbs the `\n` at
            // `datagram_end` (== start - 1) for free.
            if end - datagram_start > self.max_payload_size {
                break;
            }
            datagram_end = end;
            self.next += 1;
        }

        Some(&self.buffer[datagram_start..datagram_end])
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::backend::MetricKind;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CaptureSink(Arc<Mutex<Vec<String>>>);

    impl CaptureSink {
        /// The datagrams sent, each a complete newline-joined UDP payload.
        fn datagrams(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }

        /// All individual records across all datagrams.
        fn lines(&self) -> Vec<String> {
            self.datagrams()
                .iter()
                .flat_map(|d| d.split('\n').map(str::to_string).collect::<Vec<_>>())
                .collect()
        }
    }

    impl StatsdSink for CaptureSink {
        fn send_batch<'a>(&mut self, payloads: impl Iterator<Item = &'a str>) {
            // Copy the borrowed slices into owned Strings (the slices are only valid for the call).
            self.0.lock().unwrap().extend(payloads.map(str::to_string));
        }
    }

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

    #[test]
    fn counter_gauge_bool_encoding() {
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), Some("svc".into()));

        backend.report_start(&ReportOptions::default());
        backend.record_counter(
            &info(&arc("rx.data"), None, Unit::Count, MetricKind::Counter),
            255,
        );
        backend.record_counter(
            &info(
                &arc("rx.ecn"),
                Some(&arc("ect0")),
                Unit::Count,
                MetricKind::Counter,
            ),
            500,
        );
        backend.record_gauge(
            &info(&arc("q.depth"), None, Unit::Count, MetricKind::Gauge),
            7,
        );
        backend.record_bool(
            &info(&arc("connect"), None, Unit::Count, MetricKind::BoolCounter),
            2,
            1,
        );
        // An empty aggregation must NOT produce a bare `|#variant:` tag.
        backend.record_counter(
            &info(
                &arc("empty.agg"),
                Some(&arc("")),
                Unit::Count,
                MetricKind::Counter,
            ),
            1,
        );
        backend.report_end();

        let lines = sink.lines();
        assert!(lines.contains(&"svc.rx.data:255|c".to_string()));
        assert!(lines.contains(&"svc.rx.ecn:500|c|#variant:ect0".to_string()));
        assert!(lines.contains(&"svc.q.depth:7|g".to_string()));
        assert!(lines.contains(&"svc.connect.true:2|c".to_string()));
        assert!(lines.contains(&"svc.connect.false:1|c".to_string()));
        assert!(
            lines.contains(&"svc.empty.agg:1|c".to_string()),
            "got: {lines:?}"
        );
    }

    /// Checks whole-string tags for single legacy aggregations and per-key tags for dimension sets.
    #[test]
    fn tag_encoding_legacy_vs_dimensions() {
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);

        backend.report_start(&ReportOptions::default());
        for agg in ["Variant|ect0", "Task|worker", "Runtime|main", "send.5"] {
            backend.record_counter(
                &info(
                    &arc("legacy"),
                    Some(&arc(agg)),
                    Unit::Count,
                    MetricKind::Counter,
                ),
                1,
            );
        }
        backend.record_counter(
            &info(
                &arc("dimmed"),
                Some(&arc("operation|get,result|ok")),
                Unit::Count,
                MetricKind::Counter,
            ),
            2,
        );
        backend.record_counter(
            &info(
                &arc("one.dim"),
                Some(&arc("operation|get")),
                Unit::Count,
                MetricKind::Counter,
            ),
            3,
        );
        backend.report_end();

        let lines = sink.lines();
        assert!(lines.contains(&"legacy:1|c|#variant:Variant_ect0".to_string()));
        assert!(lines.contains(&"legacy:1|c|#variant:Task_worker".to_string()));
        assert!(lines.contains(&"legacy:1|c|#variant:Runtime_main".to_string()));
        assert!(lines.contains(&"legacy:1|c|#variant:send.5".to_string()));
        assert!(
            lines.contains(&"dimmed:2|c|#operation:get,result:ok".to_string()),
            "got: {lines:?}"
        );
        assert!(
            lines.contains(&"one.dim:3|c|#operation:get".to_string()),
            "a single keyed non-legacy tag is a dimension, not a legacy blob: {lines:?}"
        );
    }

    /// A gauge value above the `u64::MAX/1e6` float ceiling reaches statsd as the exact integer
    /// `{value}|g`, reported through a real `Registry`. A value routed through the callback path
    /// would instead be summed to f64 and `Trimmed` into the constant 18446744073709.551615.
    #[test]
    fn native_gauge_emits_exact_large_integer() {
        let registry = crate::Registry::new();
        let gauge = registry.register_gauge("queue_depth".into(), None, Unit::Count);
        gauge.set(1_783_972_929_737_661_674);

        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        registry.report(&mut backend);

        assert!(
            sink.lines()
                .contains(&"queue_depth:1783972929737661674|g".to_string()),
            "got: {:?}",
            sink.lines()
        );
    }

    /// A counter's unit is metadata: statsd doesn't render it. A `Unit::Byte` counter emits the same
    /// `:{value}|c` line as a plain counter, with no spurious `|#variant:` tag (units are not
    /// variants). This is the "backend doesn't emit units" case.
    #[test]
    fn counter_unit_is_ignored_by_statsd() {
        let registry = crate::Registry::new();
        registry
            .register_counter_with_unit("rx.bytes".into(), None, Unit::Byte)
            .increment(1500);

        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        registry.report(&mut backend);

        let lines = sink.lines();
        assert_eq!(lines, vec!["rx.bytes:1500|c".to_string()]);
    }

    /// Parses the `{value}` out of a DogStatsD histogram line of the form `prefix{value}|h|@{w}`.
    fn hist_value(line: &str, prefix: &str) -> u64 {
        assert!(
            line.starts_with(prefix),
            "line {line:?} lacks prefix {prefix:?}"
        );
        line[prefix.len()..line.find('|').unwrap()].parse().unwrap()
    }

    #[test]
    fn histogram_time_values_are_nanoseconds_not_inflated() {
        // Build a histogram via a real Summary so bucketing is genuine. record_duration stores
        // nanoseconds, so the StatsD value must be ~the nanosecond magnitude (5us -> ~5000ns),
        // NOT inflated by a us->ns multiply (which would yield ~5_000_000).
        let registry = crate::Registry::new();
        let summary =
            registry.register_summary("task.time".into(), Some("d.0".into()), Unit::Microsecond);
        summary.record_duration(std::time::Duration::from_micros(5));
        summary.record_duration(std::time::Duration::from_micros(5));
        summary.record_duration(std::time::Duration::from_micros(10));

        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), Some("svc".into()));
        registry.report(&mut backend);

        // The distribution is emitted as one `|h` sample per non-empty bucket under the base name,
        // each carrying a `@{1/count}` sample-rate weight, with the aggregation tag preserved. Two
        // samples landed in the 5us bucket and one in the 10us bucket.
        let lines: Vec<String> = sink
            .lines()
            .into_iter()
            .filter(|l| l.starts_with("svc.task.time:"))
            .collect();
        assert_eq!(lines.len(), 2, "one line per non-empty bucket: {lines:?}");
        for line in &lines {
            assert!(
                line.ends_with("|#variant:d.0"),
                "aggregation tag missing: {line}"
            );
        }

        // The histogram view reports nanosecond bucket midpoints directly; statsd must pass them
        // through unchanged. The two buckets are ~5000ns (weight 0.5, two samples) and ~10000ns
        // (weight 1, one sample) — critically nowhere near 5_000_000.
        let five = lines
            .iter()
            .find(|l| l.contains("|@0.5"))
            .expect("5us bucket with weight 0.5");
        let five_ns = hist_value(five, "svc.task.time:");
        assert!(
            (5000..5100).contains(&five_ns),
            "5us bucket should be ~5000ns, got {five_ns}"
        );
        let ten = lines
            .iter()
            .find(|l| l.contains("|@1.0|"))
            .expect("10us bucket with weight 1");
        let ten_ns = hist_value(ten, "svc.task.time:");
        assert!(
            (10000..10100).contains(&ten_ns),
            "10us bucket should be ~10000ns, got {ten_ns}"
        );
    }

    #[test]
    fn histogram_scaled_ratio_descaled() {
        // A fractional summary stores `round(scale * v)`; statsd must divide the scale back out and
        // emit the fractional sample, not the raw stored magnitude.
        let registry = crate::Registry::new();
        let summary = registry.metric("ratio").scale(1e6).summary(Unit::Count);
        // Stored as round(0.25 * 1e6) = 250_000.
        summary.record_f64(0.25);

        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        registry.report(&mut backend);

        let lines = sink.lines();
        let line = lines
            .iter()
            .find(|l| l.starts_with("ratio:"))
            .expect("ratio histogram line");
        // Parse the float `{value}` out of `ratio:{value}|h|@{w}`.
        let value: f64 = line["ratio:".len()..line.find('|').unwrap()]
            .parse()
            .unwrap();
        // ~0.25 (within bucket relative error), definitely not 250000.
        assert!(
            (0.249..=0.251).contains(&value),
            "ratio should be ~0.25, got {value}"
        );
        assert!(line.contains("|h|@"), "should be a `|h` sample: {line}");
    }

    #[test]
    fn empty_histogram_emits_nothing() {
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        backend.report_start(&ReportOptions::default());
        // a zero-count histogram view
        let buckets = [0u64; 8];
        let cfg = crate::summary::bucket::Config::new(7, 64);
        let hist = Histogram::new(&buckets, &cfg, Unit::Count, 1.0);
        backend.record_histogram(
            &info(&arc("h"), None, Unit::Count, MetricKind::Histogram),
            hist,
        );
        backend.report_end();
        assert!(sink.datagrams().is_empty());
    }

    /// Drives the `Chunks` iterator directly over a hand-built buffer + bounds, exercising packing,
    /// the oversized-record skip, and the zero-size case.
    #[test]
    fn chunks_pack_records_and_skip_oversized() {
        // Three records "aaaa", "bbbb", "toolong" laid out newline-separated as the backend would.
        // Offsets: aaaa=0..4, '\n'@4, bbbb=5..9, '\n'@9, toolong=10..17. Bounds are record ends.
        let buffer = "aaaa\nbbbb\ntoolong";
        let bounds = vec![4usize, 9, 17];

        // max 9 fits "aaaa\nbbbb" (9 bytes); "toolong" (7) fits alone in the next datagram.
        let chunks: Vec<&str> = Chunks::new(buffer, &bounds, 9).collect();
        assert_eq!(chunks, vec!["aaaa\nbbbb", "toolong"]);

        // A record longer than the limit is skipped entirely.
        let buffer2 = "waytoolongline";
        let bounds2 = vec![14usize];
        assert_eq!(Chunks::new(buffer2, &bounds2, 5).count(), 0);

        // Zero size drops everything.
        assert_eq!(Chunks::new(buffer, &bounds, 0).count(), 0);

        // Each record alone when the limit only fits one at a time.
        let chunks: Vec<&str> = Chunks::new(buffer, &bounds, 7).collect();
        assert_eq!(chunks, vec!["aaaa", "bbbb", "toolong"]);

        // A huge limit packs everything into one datagram (separators included).
        let chunks: Vec<&str> = Chunks::new(buffer, &bounds, 1000).collect();
        assert_eq!(chunks, vec!["aaaa\nbbbb\ntoolong"]);

        // An oversized record in the middle is skipped while neighbors still pack.
        // "aa"=0..2, '\n'@2, "huge"=3..7, '\n'@7, "bb"=8..10
        let buffer = "aa\nhuge\nbb";
        let bounds = vec![2usize, 7, 10];
        let chunks: Vec<&str> = Chunks::new(buffer, &bounds, 3).collect();
        assert_eq!(chunks, vec!["aa", "bb"]);
    }

    #[test]
    fn zero_values_suppressed_unless_sparse() {
        // Non-sparse: a zero counter and an all-zero bool emit nothing.
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        backend.report_start(&ReportOptions::new(false));
        backend.record_counter(&info(&arc("c"), None, Unit::Count, MetricKind::Counter), 0);
        backend.record_bool(
            &info(&arc("b"), None, Unit::Count, MetricKind::BoolCounter),
            0,
            0,
        );
        backend.report_end();
        assert!(sink.datagrams().is_empty());

        // Sparse: zeros emit (counter and both bool sides).
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        backend.report_start(&ReportOptions::new(true));
        backend.record_counter(&info(&arc("c"), None, Unit::Count, MetricKind::Counter), 0);
        backend.record_bool(
            &info(&arc("b"), None, Unit::Count, MetricKind::BoolCounter),
            0,
            0,
        );
        backend.report_end();
        let lines = sink.lines();
        assert!(lines.contains(&"c:0|c".to_string()));
        assert!(lines.contains(&"b.true:0|c".to_string()));
        assert!(lines.contains(&"b.false:0|c".to_string()));
    }

    #[test]
    fn per_metric_sparsity_overrides_report_policy() {
        use crate::backend::Sparsity;

        // Under a non-sparse report, an `AlwaysDense` counter still emits its zero, while an
        // `AlwaysSparse` counter is dropped even though the report is (report-wide) non-sparse.
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        backend.report_start(&ReportOptions::new(false));

        let dense_name = arc("dense");
        let mut dense = info(&dense_name, None, Unit::Count, MetricKind::Counter);
        dense.sparsity = Sparsity::AlwaysDense;
        backend.record_counter(&dense, 0);

        let sparse_name = arc("sparse");
        let mut sparse = info(&sparse_name, None, Unit::Count, MetricKind::Counter);
        sparse.sparsity = Sparsity::AlwaysSparse;
        backend.record_counter(&sparse, 0);

        backend.report_end();
        let lines = sink.lines();
        assert_eq!(lines, vec!["dense:0|c".to_string()]);

        // And an `AlwaysDense` all-zero bool emits both sides regardless of the report policy.
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        backend.report_start(&ReportOptions::new(false));
        let b_name = arc("b");
        let mut b = info(&b_name, None, Unit::Count, MetricKind::BoolCounter);
        b.sparsity = Sparsity::AlwaysDense;
        backend.record_bool(&b, 0, 0);
        backend.report_end();
        let lines = sink.lines();
        assert!(lines.contains(&"b.true:0|c".to_string()));
        assert!(lines.contains(&"b.false:0|c".to_string()));
    }

    #[test]
    fn bool_suppresses_zero_side_unless_sparse() {
        // Non-sparse: only the non-zero side emits (the zero side is omitted to avoid `:0|c` noise).
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        backend.report_start(&ReportOptions::new(false));
        backend.record_bool(
            &info(&arc("connect"), None, Unit::Count, MetricKind::BoolCounter),
            5,
            0,
        );
        backend.report_end();
        let lines = sink.lines();
        assert!(lines.contains(&"connect.true:5|c".to_string()));
        assert!(
            !lines.iter().any(|l| l.starts_with("connect.false")),
            "zero false side should be suppressed: {lines:?}"
        );
    }

    #[test]
    fn callback_sums_multiple_values() {
        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
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
        assert!(sink.lines().contains(&"workers:7.0|g".to_string()));
    }

    #[test]
    fn float_precision_is_limited_to_six_decimal_places() {
        // Floats in StatsD lines (sample-rate weights and scaled values) must not carry the full
        // f64 precision: strict parsers such as the CloudWatch agent reject very long decimals.
        // weight for count=3 is 1/3 ≈ 0.333333 (not 0.3333333333333333).
        let registry = crate::Registry::new();
        let summary = registry.register_summary("h".into(), None, Unit::Microsecond);
        // Record three equal samples → one bucket with count=3 → weight=1/3.
        summary.record_duration(std::time::Duration::from_micros(5));
        summary.record_duration(std::time::Duration::from_micros(5));
        summary.record_duration(std::time::Duration::from_micros(5));

        let sink = CaptureSink::default();
        let mut backend = StatsdBackend::new(sink.clone(), None);
        registry.report(&mut backend);

        let lines = sink.lines();
        let hist_line = lines
            .iter()
            .find(|l| l.starts_with("h:"))
            .expect("histogram line");
        let at = hist_line.find("|@").expect("|@ in line");
        let weight_str = &hist_line[at + 2..];
        let weight_str = weight_str.split('|').next().unwrap();
        assert_eq!(
            weight_str, "0.333333",
            "weight must be capped at 6 decimal places, not full f64 precision"
        );
    }

    #[test]
    fn trimmed_preserves_sign_for_finite_values() {
        assert_eq!(format!("{}", Trimmed(-7.0)), "-7.0");
        assert_eq!(format!("{}", Trimmed(-0.3333333333333333)), "-0.333333");
    }

    #[test]
    fn trimmed_preserves_non_finite_values() {
        assert_eq!(format!("{}", Trimmed(f64::NAN)), "NaN");
        assert_eq!(format!("{}", Trimmed(f64::INFINITY)), "inf");
        assert_eq!(format!("{}", Trimmed(f64::NEG_INFINITY)), "-inf");
    }
}
