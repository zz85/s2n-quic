// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use crate::{
    rseq::{Absorb, Channels},
    Unit,
};
use std::{sync::Arc, time::Duration};

/// The number of decimal places a backend prints for a fixed-point [`Summary::scale`].
///
/// A scale of `10^d` recovers `d` fractional digits, so the printed precision is `log10(scale)`
/// rounded to the nearest integer (clamped at 0). A scale of `1000` yields 3 places — matching the
/// historical `Unit::Percent` formatting — and the default scale of `1.0` yields 0 (integer output).
pub(crate) fn scale_decimals(scale: f64) -> usize {
    if scale <= 1.0 {
        return 0;
    }
    scale.log10().round().max(0.0) as usize
}

/// A `Summary` aggregates summary statistics. It is cheaper/smaller to add to compared to
/// `Collection` for cases where storing and reporting all individual data values may be too
/// expensive.
///
/// # Fractional values
///
/// The underlying storage counts integer bucket indices, so a `Summary` records a fixed-point view
/// of its samples: a per-metric [`scale`](Self::scale) factor maps a floating-point sample onto the
/// integer buckets at record time (via [`record_f64`](Self::record_f64)), and every backend divides
/// the bucket's representative value back out at report time. A scale of `1.0` (the default) is the
/// plain integer histogram; a scale of `1000` keeps three fractional digits, and so on. The scale
/// only sets the resolution floor (`~0.5/scale`) and ceiling (`u64::MAX/scale`) — the log-linear
/// bucketing's relative error is unchanged and magnitude-independent.
#[derive(Clone)]
pub struct Summary {
    channels: Arc<Channels<SharedSummary>>,
    idx: u32,
    display_unit: Unit,
    /// Fixed-point multiplier applied to a float sample before bucketing (and divided back out by
    /// backends). `1.0` is the plain integer histogram. See the type-level docs.
    scale: f64,
}

const BUCKETS: usize = CONFIG.total_buckets();

/// One summary's bucket counts, as the aggregate holds them.
///
/// The bucket array holds `BUCKETS` counts (about 58 KiB), so a summary allocates it when the
/// aggregate takes in its first sample, and a summary that is never sampled doesn't allocate it: a
/// process registers many summaries, and an idle one samples few of them. Once allocated, the
/// array stays for the summary's lifetime, and a report zeroes it only where a sample came since
/// the last report.
#[derive(Default)]
pub(crate) struct SharedSummary {
    /// The bucket counts; `None` until the summary's first sample.
    value: Option<Box<[u64; BUCKETS]>>,
    /// The samples counted into `value` since the last report: the sum of its buckets.
    count: u64,
}

impl SharedSummary {
    /// Counts one sample into `bucket`, allocating the bucket array at the summary's first sample.
    #[inline]
    fn absorb(&mut self, bucket: usize) {
        self.value.get_or_insert_with(zeroed_buckets)[bucket] += 1;
        self.count += 1;
    }
}

/// A zeroed bucket array, allocated on the heap without a copy on the stack.
fn zeroed_buckets() -> Box<[u64; BUCKETS]> {
    // SAFETY: Slice to array conversion doesn't change the layout of the allocation.
    //
    // FIXME: Replace with https://doc.rust-lang.org/nightly/std/boxed/struct.Box.html#method.into_array
    // once it's stabilized.
    unsafe {
        Box::from_raw(Box::into_raw(vec![0u64; BUCKETS].into_boxed_slice()) as *mut [u64; BUCKETS])
    }
}

/// The bucket array a summary reports before its first sample, shared by every such summary.
/// Nothing writes it, so its pages stay the zero pages the kernel maps for a read.
static EMPTY: [u64; BUCKETS] = [0; BUCKETS];

pub(crate) mod bucket;

// Ensure the maximum bucket fits into the space we've reserved for it.
const _: () = assert!(u16::MAX as u64 >= BUCKETS as u64);

impl Absorb for SharedSummary {
    fn handle(slots: &mut [Self], events: &mut [u64]) {
        let (chunks, tail) = events.as_chunks::<8>();
        for chunk in chunks {
            for event in chunk {
                let idx = (*event >> 16) as usize;
                slots[idx].absorb(*event as u16 as usize);
            }
        }

        for event in tail {
            let idx = (*event >> 16) as usize;
            slots[idx].absorb(*event as u16 as usize);
        }
    }
}

impl Summary {
    pub(crate) fn new(
        channels: Arc<Channels<SharedSummary>>,
        display_unit: Unit,
        scale: f64,
    ) -> Summary {
        assert!(
            scale.is_finite() && scale > 0.0,
            "summary scale must be finite and positive, got {scale}"
        );
        let idx = channels.allocate();
        Summary {
            idx,
            display_unit,
            channels,
            scale,
        }
    }

    pub(crate) fn display_unit(&self) -> Unit {
        self.display_unit
    }

    /// The fixed-point [`scale`](Self#fractional-values) applied to float samples. `1.0` for a plain
    /// integer histogram.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// Whether this summary records fractional (scaled) samples, i.e. its [`scale`](Self::scale) is
    /// not `1.0`. A consumer bridging a value that may be an integer or a float uses this to choose
    /// between [`record_value`](Self::record_value) and [`record_f64`](Self::record_f64).
    pub fn is_scaled(&self) -> bool {
        self.scale != 1.0
    }

    pub fn record_value(&self, value: u64) {
        // A scaled summary expects fractional samples via `record_f64`; recording a raw integer here
        // would bypass the scale and land in the wrong bucket. Guard in debug so the two recording
        // modes can't be silently mixed on one handle.
        debug_assert!(
            self.scale == 1.0,
            "record_value on a scaled summary (scale={}); use record_f64",
            self.scale
        );
        self.record_bucketed(value);
    }

    /// Records a floating-point `value` into the fixed-point histogram, mapping it onto the integer
    /// buckets via this summary's [`scale`](Self::scale).
    ///
    /// `NaN` is dropped, negative values are clamped to `0` (the buckets are unsigned), and the
    /// scaled magnitude saturates at `u64::MAX` rather than wrapping. Values below the resolution
    /// floor (`~0.5/scale`) round to `0`.
    pub fn record_f64(&self, value: f64) {
        if value.is_nan() {
            return;
        }
        // Clamp to the representable non-negative range before the cast: `as u64` already saturates
        // (negative -> 0, +inf/too-large -> u64::MAX), but rounding first keeps the fixed-point
        // mapping symmetric.
        let scaled = (value * self.scale).round();
        let value = if scaled <= 0.0 { 0 } else { scaled as u64 };
        self.record_bucketed(value);
    }

    /// Sends the already-integer `value` to its bucket. Shared by [`record_value`] and the scaled
    /// [`record_f64`] so the guard lives only on the public integer entry point.
    #[inline]
    fn record_bucketed(&self, value: u64) {
        let Some(bucket) = CONFIG.value_to_index(value) else {
            return;
        };
        self.channels
            .send_event(((self.idx as u64) << 16) | bucket as u64);
    }

    pub fn record_duration(&self, duration: Duration) {
        assert!(matches!(
            self.display_unit,
            Unit::Microsecond | Unit::Second
        ));
        debug_assert!(
            self.scale == 1.0,
            "record_duration on a scaled summary (scale={})",
            self.scale
        );
        self.record_bucketed(duration.as_nanos() as u64);
    }

    /// Reports the histogram to `backend` via a borrowed [`Histogram`](crate::Histogram) view, then
    /// drains the buckets. The view borrows the live bucket array, or `EMPTY` before the
    /// summary's first sample, and is only valid for the duration of the `record_histogram` call
    /// (which happens under the aggregate lock). Its count is the one the summary kept, so the
    /// report doesn't sum the buckets, and a report with no sample since the last one leaves the
    /// buckets as they are, all zero.
    pub(crate) fn report(&self, info: &crate::MetricInfo<'_>, backend: &mut dyn crate::Backend) {
        self.channels.get_mut(self.idx, |hist| {
            let buckets: &[u64; BUCKETS] = hist.value.as_deref().unwrap_or(&EMPTY);
            let view = crate::backend::Histogram::with_count(
                buckets.as_slice(),
                hist.count,
                &CONFIG,
                self.display_unit,
                self.scale,
            );
            backend.record_histogram(info, view);
            if hist.count != 0 {
                if let Some(value) = hist.value.as_mut() {
                    value.as_mut_slice().fill(0);
                }
                hist.count = 0;
            }
        });
    }
}

pub(crate) const CONFIG: bucket::Config = bucket::Config::new(7, 64);

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn count_correct() {
        let registry = crate::Registry::new();
        let summary = registry.register_summary(String::from("a"), None, Unit::Count);
        assert_eq!(registry.take_current_metrics_line(), "a=0");

        summary.record_value(0);
        summary.record_value(10);
        summary.record_value(20);
        summary.record_value(30);
        assert_eq!(registry.take_current_metrics_line(), "a=0*1+10*1+20*1+30*1");
        assert_eq!(registry.take_current_metrics_line(), "a=0");
    }

    #[test]
    fn visits_all_buckets() {
        let registry = crate::Registry::new();
        let summary = registry.register_summary(String::from("a"), None, Unit::Count);

        for bucket in 0..CONFIG.total_buckets() {
            let start = CONFIG.index_to_lower_bound(bucket);
            // Record a value from every bucket.
            summary.record_value(start);
        }

        assert_eq!(
            registry.take_current_metrics_line(),
            "a=0*1+3687*742+209407*742+11763711*743+643825663*742+34292629503*742+1979979923455*743+112425063940095*742+6315594789945343*743+345651271400685567*742+2531022990582218751*371+13078453317883920383*297+17906312118425092095*67+18410715276690587647*7"
        );
    }

    #[test]
    fn maximum() {
        let registry = crate::Registry::new();
        let summary = registry.register_summary(String::from("a"), None, Unit::Count);
        summary.record_value(u64::MAX);
        assert_eq!(
            registry.take_current_metrics_line(),
            "a=18410715276690587647*1"
        );
    }

    #[test]
    fn sparse_skipped() {
        let registry = crate::Registry::new();
        let summary = registry.register_summary(String::from("a"), None, Unit::Byte);
        assert_eq!(
            registry
                .try_take_current_metrics_line_sparse(false)
                .unwrap(),
            ""
        );

        summary.record_value(1);

        assert_eq!(
            registry
                .try_take_current_metrics_line_sparse(false)
                .unwrap(),
            "a=1*1 B"
        );

        assert_eq!(
            registry.try_take_current_metrics_line_sparse(true).unwrap(),
            "a=0 B"
        );
    }

    #[test]
    fn config() {
        assert_eq!(CONFIG.total_buckets(), 7424);
    }

    #[test]
    fn scale_decimals_matches_log10() {
        assert_eq!(scale_decimals(1.0), 0);
        assert_eq!(scale_decimals(0.5), 0);
        assert_eq!(scale_decimals(10.0), 1);
        assert_eq!(scale_decimals(1000.0), 3);
        assert_eq!(scale_decimals(1e6), 6);
    }

    /// A scaled summary records fractional values via `record_f64` and the querylog line prints the
    /// de-scaled value with `log10(scale)` decimal places, within the histogram's relative error.
    #[test]
    fn record_f64_round_trips_through_scale() {
        let registry = crate::Registry::new();
        let summary = registry.metric("ratio").scale(1e6).summary(Unit::Count);

        // A compression ratio compressing to a quarter.
        summary.record_f64(0.25);

        let line = registry.take_current_metrics_line();
        // Format is `ratio={value:.6}*{count}` — one bucket, one sample.
        let value: f64 = line["ratio=".len()..line.find('*').unwrap()]
            .parse()
            .unwrap();
        assert!(
            (0.249..=0.251).contains(&value),
            "expected ~0.25, got {value} (line {line:?})"
        );
    }

    /// Small fractions below `1/scale` still land in distinct low buckets (not collapsed to 0) as
    /// long as they're above the `~0.5/scale` resolution floor.
    #[test]
    fn record_f64_preserves_small_fractions() {
        let registry = crate::Registry::new();
        let summary = registry.metric("r").scale(1e6).summary(Unit::Count);
        // 0.001 -> round(1e6 * 0.001) = 1000, well above the floor.
        summary.record_f64(0.001);
        let line = registry.take_current_metrics_line();
        let value: f64 = line["r=".len()..line.find('*').unwrap()].parse().unwrap();
        assert!(
            (0.0009..=0.0011).contains(&value),
            "expected ~0.001, got {value} (line {line:?})"
        );
    }

    /// `record_f64` drops `NaN`, clamps negatives to the zero bucket, and saturates an
    /// astronomically large scaled magnitude into the top bucket rather than wrapping.
    #[test]
    fn record_f64_handles_degenerate_inputs() {
        let registry = crate::Registry::new();
        let summary = registry.metric("d").scale(1e6).summary(Unit::Count);

        // NaN is dropped entirely: nothing recorded.
        summary.record_f64(f64::NAN);
        assert_eq!(
            registry
                .try_take_current_metrics_line_sparse(false)
                .unwrap(),
            "",
            "NaN must not record a sample"
        );

        // Negative clamps to the zero bucket; +inf saturates to the max bucket. Two samples.
        summary.record_f64(-5.0);
        summary.record_f64(f64::INFINITY);
        let line = registry.take_current_metrics_line();
        // Two buckets: the 0 bucket (de-scaled 0.0) first, then the saturated top bucket.
        assert!(line.starts_with("d=0.000000*1+"), "line {line:?}");
        // The saturated top value (u64::MAX / 1e6) is enormous — not a wrap to something small.
        let top: f64 = line
            .rsplit_once('+')
            .unwrap()
            .1
            .split_once('*')
            .unwrap()
            .0
            .parse()
            .unwrap();
        assert!(top > 1e12, "saturated top bucket should be huge, got {top}");
    }

    /// Registering the same summary twice with conflicting scales is a programming error.
    #[test]
    #[should_panic(expected = "different scale")]
    fn conflicting_scale_panics() {
        let registry = crate::Registry::new();
        registry.metric("a").scale(1000.0).summary(Unit::Count);
        registry.metric("a").scale(1e6).summary(Unit::Count);
    }

    /// Recording a raw integer into a scaled summary trips the debug guard (the two recording modes
    /// must not be mixed on one handle).
    #[test]
    #[should_panic(expected = "record_f64")]
    #[cfg(debug_assertions)]
    fn record_value_on_scaled_summary_panics() {
        let registry = crate::Registry::new();
        let summary = registry.metric("a").scale(1e6).summary(Unit::Count);
        summary.record_value(3);
    }

    /// An unscaled summary (the default) is byte-identical to before: integer output, no decimals.
    #[test]
    fn unscaled_summary_is_integer() {
        let registry = crate::Registry::new();
        let summary = registry.register_summary("a".into(), None, Unit::Count);
        assert_eq!(summary.scale(), 1.0);
        assert!(!summary.is_scaled());
        summary.record_value(42);
        assert_eq!(registry.take_current_metrics_line(), "a=42*1");
    }

    /// A summary allocates its bucket array when the aggregate takes in its first sample.
    mod buckets {
        use crate::{Backend, CallbackValue, Histogram, MetricInfo, Registry, Summary, Unit};
        use std::collections::BTreeMap;

        /// Each histogram a report records, by name: its count, and the sum of its buckets.
        #[derive(Default)]
        struct Capture {
            histograms: BTreeMap<String, (u64, u64)>,
        }

        impl Backend for Capture {
            fn record_counter(&mut self, _: &MetricInfo<'_>, _: u64) {}
            fn record_gauge(&mut self, _: &MetricInfo<'_>, _: i64) {}
            fn record_bool(&mut self, _: &MetricInfo<'_>, _: u64, _: u64) {}
            fn record_histogram(&mut self, info: &MetricInfo<'_>, hist: Histogram<'_>) {
                let sum = hist.raw_buckets().iter().sum();
                let name = info.name.to_string();
                self.histograms.insert(name, (hist.count(), sum));
            }
            fn record_callback(&mut self, _: &MetricInfo<'_>, _: &[&dyn CallbackValue]) {}
        }

        /// Reports `registry`, and returns each histogram it records.
        fn report(registry: &Registry) -> BTreeMap<String, (u64, u64)> {
            let mut capture = Capture::default();
            registry.report(&mut capture);
            capture.histograms
        }

        /// Whether `summary` holds its bucket array.
        fn allocated(summary: &Summary) -> bool {
            let Summary { channels, idx, .. } = summary;
            channels.get_mut(*idx, |shared| shared.value.is_some())
        }

        /// Summaries that never take a sample report a histogram of no samples, and none of them
        /// allocates its bucket array. A summary that takes samples allocates its array once and
        /// reports its samples, its count the sum of its buckets; the report drains them, and the
        /// array stays for the next samples.
        #[test]
        fn allocated_at_first_sample() {
            let registry = Registry::new();
            let idle: Vec<Summary> = (0..64)
                .map(|i| registry.register_summary(format!("idle.{i}"), None, Unit::Count))
                .collect();
            let busy = registry.register_summary("busy".into(), None, Unit::Count);

            let reported = report(&registry);
            assert_eq!(reported.len(), 65);
            assert!(reported.values().all(|counts| *counts == (0, 0)));
            assert!(!allocated(&busy));
            assert!(idle.iter().all(|summary| !allocated(summary)));

            for value in [10, 10, 20, 30] {
                busy.record_value(value);
            }
            let mut reported = report(&registry);
            assert_eq!(reported.remove("busy"), Some((4, 4)));
            assert!(reported.values().all(|counts| *counts == (0, 0)));
            assert!(allocated(&busy));
            assert!(idle.iter().all(|summary| !allocated(summary)));

            // The report drained the samples: the next one counts none, and every bucket of the
            // array the summary keeps is zero.
            let reported = report(&registry);
            assert!(reported.values().all(|counts| *counts == (0, 0)));
            assert!(allocated(&busy));

            busy.record_value(5);
            let mut reported = report(&registry);
            assert_eq!(reported.remove("busy"), Some((1, 1)));
            assert!(reported.values().all(|counts| *counts == (0, 0)));
        }
    }
}
