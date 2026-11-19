// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Tag types for metrics — two distinct concepts that share a `(key, value)` shape.
//!
//! # Aggregation tags ([`AggregationTags`], a borrowed view)
//!
//! A metric's *emitted* dimension is the free-form `aggregation: Option<Arc<str>>` carried on its
//! [`MetricKey`](crate::registry) and surfaced through
//! [`MetricInfo::aggregation`](crate::MetricInfo). By convention that string is a `key|value` pair
//! (`Variant|ect0`, `Task|foo`, `Runtime|bar`) — the form
//! [`counters_for_enum!`](crate::counters_for_enum) produces and the Prometheus backend already
//! parses into labels. A bare string with no `|` (e.g. the `send.5` variant the dc runtime records
//! via `register_nominal`) is a single *keyless* dimension. [`AggregationTags`] is a **borrowed,
//! parsed view** over that string: allocation-free, changing nothing about how aggregation is stored
//! or emitted. It lets a policy or backend query the individual aggregation dimensions (e.g. drop
//! the `worker` key when collapsing) instead of re-implementing the split. [`serialize`] is the
//! inverse. Obtained from [`MetricInfo::aggregation_tags`](crate::MetricInfo::aggregation_tags).
//!
//! # Metric tags ([`MetricTags`], stored metadata)
//!
//! Separately, a metric may carry a set of `(key, value)` **metadata** tags attached at
//! registration (e.g. `level=debug`), stored on the metric entry like `unit`/`sparsity` and
//! surfaced through [`MetricInfo::tags`](crate::MetricInfo). These are **metadata-only**: existing
//! backends ignore them, so they add no wire cardinality. A policy matches on them to route,
//! filter, or collapse a metric (e.g. "collapse everything tagged `level=debug` for statsd"), and a
//! backend may later opt in to emit them. [`MetricTags`] is a borrowed slice view over the stored
//! pairs.
//!
//! The two are deliberately named apart — the *emitted* dimension is `aggregation_tags()` returning
//! `AggregationTags`, while the *metadata* is the plain `tags` field returning `MetricTags` — so a
//! backend author reaching for `info.tags` gets the metadata, not the wire dimension.

use std::sync::Arc;

/// Separates one `key|value` tag from the next within an aggregation string. A tag key or value
/// must not contain this character (or [`KV_SEPARATOR`]); the aggregation grammar is un-escaped, so
/// an embedded separator would be re-parsed as a tag boundary. In-tree aggregation values
/// (`send.{idx}`, `Variant|{variant}`) never do, and the querylog format already precludes commas.
pub const TAG_SEPARATOR: char = ',';

/// Matches the keyless, `Variant`, `Task`, and `Runtime` aggregation keys.
pub fn is_legacy_tag_key(key: &str) -> bool {
    matches!(key, "" | "Variant" | "Task" | "Runtime")
}

/// Separates a tag's key from its value. See the escaping note on [`TAG_SEPARATOR`].
pub const KV_SEPARATOR: char = '|';

/// A single parsed aggregation tag. `key` is empty for a *keyless* (bare, legacy) dimension such as
/// `send.5`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AggregationTag<'a> {
    pub key: &'a str,
    pub value: &'a str,
}

/// A borrowed, parsed view over an aggregation string's tags.
///
/// Parsing is lazy (each [`iter`](Self::iter) call re-splits the borrowed string), so constructing
/// an `AggregationTags` and reading it allocates nothing. This is the read path. Distinct from
/// [`MetricTags`], which is the stored *metadata* on a metric — see the module docs.
#[derive(Clone, Copy, Debug)]
pub struct AggregationTags<'a> {
    raw: &'a str,
}

impl<'a> AggregationTags<'a> {
    /// Parses `aggregation` (the raw convention string) into a tag view. An empty string yields no
    /// tags.
    pub fn parse(aggregation: &'a str) -> Self {
        AggregationTags { raw: aggregation }
    }

    /// The raw underlying aggregation string.
    pub fn as_str(&self) -> &'a str {
        self.raw
    }

    /// Whether there are no tags (an empty aggregation string).
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Iterates the individual `(key, value)` tags. A segment with no `|` yields an
    /// [`AggregationTag`] with an empty `key` (a keyless/bare dimension). Empty segments (e.g. from
    /// a stray separator) are skipped.
    pub fn iter(&self) -> impl Iterator<Item = AggregationTag<'a>> + 'a {
        self.raw
            .split(TAG_SEPARATOR)
            .filter(|part| !part.is_empty())
            .map(|part| match part.split_once(KV_SEPARATOR) {
                Some((key, value)) => AggregationTag { key, value },
                None => AggregationTag {
                    key: "",
                    value: part,
                },
            })
    }

    /// The value of the tag with the given `key`, or `None` if absent. Matches keys exactly.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.iter().find(|t| t.key == key).map(|t| t.value)
    }

    /// Whether any tag has the given `key`.
    pub fn contains_key(&self, key: &str) -> bool {
        self.iter().any(|t| t.key == key)
    }
}

/// Renders a set of `(key, value)` tags into the canonical aggregation string: sorted by
/// `(key, value)` and joined with [`TAG_SEPARATOR`], each tag written as `key|value` (or bare
/// `value` for an empty key). Returns `None` when there are no tags, so the result maps directly
/// onto the `Option<Arc<str>>` aggregation.
///
/// Sorting makes the output canonical: any two tag sets with the same members serialize to the
/// same string, so a downstream aggregator keying on the residual string groups them identically.
/// A single keyless tag round-trips a legacy bare string unchanged (`[("", "send.5")]` -> `send.5`),
/// and a single `key|value` tag round-trips the historical convention (`Variant|ect0`).
pub fn serialize<'a>(tags: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<String> {
    let mut tags: Vec<(&str, &str)> = tags.into_iter().collect();
    if tags.is_empty() {
        return None;
    }
    tags.sort_unstable();
    let mut out = String::new();
    for (i, (key, value)) in tags.iter().enumerate() {
        if i != 0 {
            out.push(TAG_SEPARATOR);
        }
        if key.is_empty() {
            out.push_str(value);
        } else {
            out.push_str(key);
            out.push(KV_SEPARATOR);
            out.push_str(value);
        }
    }
    Some(out)
}

/// [`serialize`], returning an `Arc<str>` ready to store as an aggregation.
pub(crate) fn serialize_arc<'a>(
    tags: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<Arc<str>> {
    serialize(tags).map(Arc::from)
}

/// The stored representation of a metric's metadata tags: a shared, sorted slice of
/// `(key, value)` pairs.
///
/// Shared via `Arc` so cloning a metric's tags into [`MetricInfo`](crate::MetricInfo) every report
/// is a refcount bump, and empty (the common untagged case) allocates nothing. Kept sorted by key
/// so equality/lookup is stable and two registrations of the same logical tag set compare equal.
pub type MetricTagSet = Arc<[(Arc<str>, Arc<str>)]>;

/// The shared empty [`MetricTagSet`], for the common untagged metric. Cloning it is a refcount
/// bump — no per-registration allocation.
pub(crate) fn empty_metric_tag_set() -> MetricTagSet {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<MetricTagSet> = OnceLock::new();
    EMPTY.get_or_init(|| Arc::from(Vec::new())).clone()
}

/// Builds a sorted [`MetricTagSet`] from `(key, value)` pairs. A later pair with a duplicate key
/// overrides an earlier one (last-write-wins), matching a builder that sets the same key twice.
pub(crate) fn metric_tag_set<K, V>(tags: impl IntoIterator<Item = (K, V)>) -> MetricTagSet
where
    K: Into<Arc<str>>,
    V: Into<Arc<str>>,
{
    let mut pairs: Vec<(Arc<str>, Arc<str>)> = tags
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    // Stable sort by key so equal keys keep their insertion order; then collapse a run of equal
    // keys down to its *last* entry (last-write-wins). `dedup_by` keeps the first of a run, so
    // reverse first, dedup (keeping what was originally last), and reverse back to sorted order.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs.reverse();
    pairs.dedup_by(|a, b| a.0 == b.0);
    pairs.reverse();
    Arc::from(pairs.into_boxed_slice())
}

/// A borrowed view over a metric's stored metadata tags.
///
/// Cheap (`Copy`) and read-only; obtained from [`MetricInfo::tags`](crate::MetricInfo). Tags are
/// kept sorted by key.
#[derive(Clone, Copy, Debug)]
pub struct MetricTags<'a> {
    pairs: &'a [(Arc<str>, Arc<str>)],
}

impl<'a> MetricTags<'a> {
    /// A view over the given stored pairs.
    pub fn new(pairs: &'a [(Arc<str>, Arc<str>)]) -> Self {
        MetricTags { pairs }
    }

    /// An empty tag view.
    pub fn empty() -> Self {
        MetricTags { pairs: &[] }
    }

    /// Whether the metric carries no tags.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Iterates the `(key, value)` tags (sorted by key).
    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        self.pairs.iter().map(|(k, v)| (k.as_ref(), v.as_ref()))
    }

    /// The value of the tag with the given `key`, or `None`.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.pairs
            .iter()
            .find(|(k, _)| k.as_ref() == key)
            .map(|(_, v)| v.as_ref())
    }

    /// Whether a tag with the given `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k.as_ref() == key)
    }

    /// Whether a tag with the given `key` equals `value`.
    pub fn matches(&self, key: &str, value: &str) -> bool {
        self.get(key) == Some(value)
    }

    /// Clones this borrowed view into an owned [`MetricTagSet`] (the pairs are already sorted).
    ///
    /// The common untagged case returns the shared empty set (a refcount bump, no allocation);
    /// only a non-empty view allocates.
    pub fn to_set(&self) -> MetricTagSet {
        if self.pairs.is_empty() {
            empty_metric_tag_set()
        } else {
            Arc::from(self.pairs.to_vec().into_boxed_slice())
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn collect(agg: &str) -> Vec<(&str, &str)> {
        AggregationTags::parse(agg)
            .iter()
            .map(|t| (t.key, t.value))
            .collect()
    }

    #[test]
    fn parses_keyed_and_keyless_tags() {
        // The historical single `key|value` convention.
        assert_eq!(collect("Variant|ect0"), vec![("Variant", "ect0")]);
        // A bare legacy dimension parses as one keyless tag.
        assert_eq!(collect("send.5"), vec![("", "send.5")]);
        // Multiple tags, comma-joined.
        assert_eq!(
            collect("Variant|ect0,worker|send.5"),
            vec![("Variant", "ect0"), ("worker", "send.5")]
        );
        // Empty aggregation yields no tags.
        assert!(AggregationTags::parse("").is_empty());
        assert_eq!(collect(""), Vec::<(&str, &str)>::new());
    }

    #[test]
    fn get_and_contains_key() {
        let tags = AggregationTags::parse("worker|send.5,Variant|ect0");
        assert_eq!(tags.get("worker"), Some("send.5"));
        assert_eq!(tags.get("Variant"), Some("ect0"));
        assert_eq!(tags.get("missing"), None);
        assert!(tags.contains_key("worker"));
        assert!(!tags.contains_key("nope"));
        // A keyless tag is addressable under the empty key.
        assert_eq!(AggregationTags::parse("send.5").get(""), Some("send.5"));
    }

    #[test]
    fn serialize_is_canonical_and_sorted() {
        // Sorted by (key, value) regardless of input order.
        assert_eq!(
            serialize([("worker", "send.5"), ("Variant", "ect0")]).as_deref(),
            Some("Variant|ect0,worker|send.5")
        );
        // A keyless tag serializes bare.
        assert_eq!(serialize([("", "send.5")]).as_deref(), Some("send.5"));
        // No tags -> None (maps onto Option<Arc<str>> aggregation).
        assert_eq!(serialize(std::iter::empty()), None);
    }

    #[test]
    fn round_trips_existing_conventions() {
        for agg in ["Variant|ect0", "Task|foo", "Runtime|A", "send.5"] {
            let pairs: Vec<(&str, &str)> = AggregationTags::parse(agg)
                .iter()
                .map(|t| (t.key, t.value))
                .collect();
            assert_eq!(serialize(pairs).as_deref(), Some(agg), "round-trip {agg:?}");
        }
    }

    #[test]
    fn metric_tag_set_is_sorted_and_last_write_wins() {
        // Sorted by key regardless of insertion order.
        let set = metric_tag_set([("level", "debug"), ("component", "tx")]);
        let pairs: Vec<(&str, &str)> = MetricTags::new(&set).iter().collect();
        assert_eq!(pairs, vec![("component", "tx"), ("level", "debug")]);

        // A duplicate key keeps the last value.
        let set = metric_tag_set([("level", "debug"), ("level", "info")]);
        assert_eq!(MetricTags::new(&set).get("level"), Some("info"));
        assert_eq!(MetricTags::new(&set).iter().count(), 1);

        // Empty stays empty (and allocates a zero-length slice).
        let set = metric_tag_set(Vec::<(&str, &str)>::new());
        assert!(MetricTags::new(&set).is_empty());
    }

    #[test]
    fn metric_tags_lookup() {
        let set = metric_tag_set([("level", "debug"), ("component", "tx")]);
        let tags = MetricTags::new(&set);
        assert!(tags.contains_key("level"));
        assert!(tags.matches("level", "debug"));
        assert!(!tags.matches("level", "info"));
        assert_eq!(tags.get("component"), Some("tx"));
        assert_eq!(tags.get("missing"), None);
        assert!(MetricTags::empty().is_empty());
    }
}
