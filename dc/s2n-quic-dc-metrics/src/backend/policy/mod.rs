// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! A [`Backend`] wrapper that filters and pre-aggregates metrics per a [`Policy`], in front of any
//! inner backend.
//!
//! # Why
//!
//! `s2n-quic-dc` registers the same metric name once per worker/sender with a distinct
//! [aggregation](crate::MetricInfo::aggregation) variant (`worker|send.5`, …). Every such series is
//! its own line on the wire, so per-worker metrics multiply cardinality and throttle downstreams
//! like CloudWatch. [`Filtered`] lets a deployment collapse that cardinality **per backend** — e.g.
//! statsd collapses per-worker loss into one series while the querylog/tracing path keeps every
//! worker for debugging — without touching any existing backend: the wrapper feeds an inner backend
//! already-collapsed records through the normal [`Backend`] calls.
//!
//! # Model
//!
//! A [`Policy`] is a set of rules. Each rule has a [`Matcher`] and an [`Action`]:
//! [`Keep`](Action::Keep) (forward verbatim), [`Drop`](Action::Drop) (filter the metric out of this
//! backend entirely), or [`Collapse`](Action::Collapse) (merge the metric across one aggregation key
//! — summing counters, bool sides, gauge/callback values, and merging histogram buckets — and
//! re-emit one series with that key removed). [`FlattenDimensions`](Action::FlattenDimensions)
//! emits a series per dimension combination and a combined series under the original name.
//!
//! A [`Matcher`] is a **predicate tree** over three leaf kinds — the metric name
//! ([`name`](Matcher::name)/[`prefix`](Matcher::prefix)/[`glob`](Matcher::glob)), its
//! [metadata tags](crate::MetricTags)
//! ([`tag`](Matcher::tag)/[`tag_present`](Matcher::tag_present)/[`tag_any_of`](Matcher::tag_any_of)/[`tag_glob`](Matcher::tag_glob)),
//! and its emitted aggregation keys ([`agg_key`](Matcher::agg_key)) — combined with
//! [`all`](Matcher::all)/[`and`](Matcher::and) (AND), [`any_of`](Matcher::any_of)/[`or`](Matcher::or)
//! (OR), and [`not`](Matcher::not) (NOT). So `send.* AND level=debug`, `send.* OR recv.*`, and
//! `NOT tx.acked.frame.*` are all expressible.
//!
//! Each rule has an explicit **priority**; the highest-priority matching rule wins (ties broken by
//! later declaration). Convenience constructors default a rule's priority from its matcher's shape
//! (exact name > prefix > glob > tag/aggregation-only > broad), so the common case needs no manual
//! ordering, but [`rule_with_priority`](Policy::rule_with_priority) overrides it. A metric matched by
//! no rule passes straight through unchanged; an explicit high-priority [`Keep`](Action::Keep) opts
//! a metric back in over a broader `Drop`/`Collapse`. Resolution is memoized per distinct metric
//! identity, so the rule scan runs once per metric no matter how many reports pass.

use crate::{
    backend::{Backend, CallbackValue, Histogram, MetricInfo, MetricKind, ReportOptions, Sparsity},
    Unit,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

mod dsl;
pub use dsl::ParsePolicyError;

/// How a string (a metric name, or a tag value) is matched.
#[derive(Clone, Debug)]
enum StrMatch {
    /// Matches any string (used for "key present with any value", e.g. `level=*`).
    Any,
    /// Matches exactly.
    Exact(Arc<str>),
    /// Matches any of a set of exact values (e.g. `level = debug|info|warn`).
    OneOf(Vec<Arc<str>>),
    /// Matches a prefix (e.g. name `send.`).
    Prefix(Arc<str>),
    /// Matches a `*`-glob (e.g. name `*.lost`, or value `warn*`).
    Glob(Arc<str>),
}

impl StrMatch {
    fn matches(&self, s: &str) -> bool {
        match self {
            StrMatch::Any => true,
            StrMatch::Exact(v) => s == v.as_ref(),
            StrMatch::OneOf(vs) => vs.iter().any(|v| s == v.as_ref()),
            StrMatch::Prefix(p) => s.starts_with(p.as_ref()),
            StrMatch::Glob(g) => glob_match(g, s),
        }
    }

    /// A specificity score, higher = matches a narrower set. Feeds a rule's default priority.
    fn specificity(&self) -> u32 {
        match self {
            // A longer exact/prefix is more specific; bias exact above prefix above glob.
            StrMatch::Exact(v) => 1000 + v.len() as u32,
            StrMatch::OneOf(vs) => 900 + vs.len() as u32,
            StrMatch::Prefix(p) => 500 + p.len() as u32,
            StrMatch::Glob(_) => 300,
            StrMatch::Any => 100,
        }
    }
}

/// A single predicate leaf: a constraint on one dimension of a metric.
#[derive(Clone, Debug)]
enum Leaf {
    /// Matches every metric (an unconstrained leaf).
    Always,
    /// Constrains the metric name.
    Name(StrMatch),
    /// Constrains a [metadata tag](crate::MetricTags): the key must be present and its value match.
    Tag { key: Arc<str>, value: StrMatch },
    /// Requires the emitted [aggregation](crate::MetricInfo::aggregation) to carry the given key.
    AggKey(Arc<str>),
    /// Requires the emitted aggregation to carry at least one non-legacy (dimension) key — any
    /// key [`is_legacy_tag_key`](crate::tags::is_legacy_tag_key) rejects.
    AnyDimensionKey,
}

impl Leaf {
    fn matches(&self, info: &MetricInfo<'_>) -> bool {
        match self {
            Leaf::Always => true,
            Leaf::Name(m) => m.matches(info.name),
            Leaf::Tag { key, value } => match info.tags.get(key) {
                Some(v) => value.matches(v),
                None => false,
            },
            Leaf::AggKey(key) => info.aggregation_tags().contains_key(key),
            Leaf::AnyDimensionKey => info
                .aggregation_tags()
                .iter()
                .any(|t| !crate::tags::is_legacy_tag_key(t.key)),
        }
    }

    fn specificity(&self) -> u32 {
        match self {
            Leaf::Always => 0,
            Leaf::Name(m) => m.specificity(),
            // A tag value predicate: reuse the string specificity, scaled below name so a name
            // constraint dominates when both are present in an AND.
            Leaf::Tag { value, .. } => 200 + value.specificity() / 4,
            Leaf::AggKey(_) => 200,
            Leaf::AnyDimensionKey => 200,
        }
    }
}

/// A predicate tree selecting the metrics a [`Policy`] rule applies to.
///
/// Built from leaf constructors ([`always`](Self::always), name
/// [`name`](Self::name)/[`prefix`](Self::prefix)/[`glob`](Self::glob), tag
/// [`tag`](Self::tag)/[`tag_present`](Self::tag_present)/[`tag_any_of`](Self::tag_any_of)/[`tag_glob`](Self::tag_glob),
/// and [`agg_key`](Self::agg_key)) combined with [`all`](Self::all)/[`and`](Self::and),
/// [`any_of`](Self::any_of)/[`or`](Self::or), and [`not`](Self::not). Evaluation is a straightforward
/// recursive walk; there is no per-metric allocation.
#[derive(Clone, Debug)]
pub struct Matcher {
    node: Node,
}

#[derive(Clone, Debug)]
enum Node {
    Leaf(Leaf),
    /// Conjunction — all children must match. An empty `All` matches everything.
    All(Vec<Node>),
    /// Disjunction — any child matches. An empty `Any` matches nothing.
    Any(Vec<Node>),
    /// Negation.
    Not(Box<Node>),
}

impl Node {
    fn matches(&self, info: &MetricInfo<'_>) -> bool {
        match self {
            Node::Leaf(leaf) => leaf.matches(info),
            Node::All(children) => children.iter().all(|c| c.matches(info)),
            Node::Any(children) => children.iter().any(|c| c.matches(info)),
            Node::Not(child) => !child.matches(info),
        }
    }

    /// Default specificity: an AND is as specific as its most specific child (the tightest
    /// constraint dominates), plus a small bonus per extra constraint; an OR is as specific as its
    /// *least* specific child (it matches the widest of them); a NOT matches broadly.
    fn specificity(&self) -> u32 {
        match self {
            Node::Leaf(leaf) => leaf.specificity(),
            Node::All(children) => {
                let max = children.iter().map(Node::specificity).max().unwrap_or(0);
                max + children.len().saturating_sub(1) as u32
            }
            Node::Any(children) => children.iter().map(Node::specificity).min().unwrap_or(0),
            Node::Not(_) => 50,
        }
    }
}

impl Matcher {
    fn leaf(leaf: Leaf) -> Self {
        Matcher {
            node: Node::Leaf(leaf),
        }
    }

    /// Matches every metric.
    pub fn always() -> Self {
        Matcher::leaf(Leaf::Always)
    }

    /// Matches a metric name exactly.
    pub fn name(name: impl Into<Arc<str>>) -> Self {
        Matcher::leaf(Leaf::Name(StrMatch::Exact(name.into())))
    }

    /// Matches metric names beginning with `prefix` (e.g. `send.`).
    pub fn prefix(prefix: impl Into<Arc<str>>) -> Self {
        Matcher::leaf(Leaf::Name(StrMatch::Prefix(prefix.into())))
    }

    /// Matches metric names against a `*`-glob (e.g. `*.lost`, `tx.acked.frame.*`).
    pub fn glob(pattern: impl Into<Arc<str>>) -> Self {
        Matcher::leaf(Leaf::Name(StrMatch::Glob(pattern.into())))
    }

    /// Matches metrics whose [metadata tag](crate::MetricTags) `key` equals `value`.
    pub fn tag(key: impl Into<Arc<str>>, value: impl Into<Arc<str>>) -> Self {
        Matcher::leaf(Leaf::Tag {
            key: key.into(),
            value: StrMatch::Exact(value.into()),
        })
    }

    /// Matches metrics whose [metadata tag](crate::MetricTags) `key` is present with any value
    /// (`key=*`).
    pub fn tag_present(key: impl Into<Arc<str>>) -> Self {
        Matcher::leaf(Leaf::Tag {
            key: key.into(),
            value: StrMatch::Any,
        })
    }

    /// Matches metrics whose [metadata tag](crate::MetricTags) `key` equals any of `values`
    /// (`key = a|b|c`).
    pub fn tag_any_of<V: Into<Arc<str>>>(
        key: impl Into<Arc<str>>,
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        Matcher::leaf(Leaf::Tag {
            key: key.into(),
            value: StrMatch::OneOf(values.into_iter().map(Into::into).collect()),
        })
    }

    /// Matches metrics whose [metadata tag](crate::MetricTags) `key`'s value matches a `*`-glob
    /// (e.g. `key = warn*`).
    pub fn tag_glob(key: impl Into<Arc<str>>, pattern: impl Into<Arc<str>>) -> Self {
        Matcher::leaf(Leaf::Tag {
            key: key.into(),
            value: StrMatch::Glob(pattern.into()),
        })
    }

    /// Matches metrics whose emitted [aggregation](crate::MetricInfo::aggregation) carries the given
    /// key.
    pub fn agg_key(key: impl Into<Arc<str>>) -> Self {
        Matcher::leaf(Leaf::AggKey(key.into()))
    }

    /// Matches metrics whose emitted aggregation carries at least one dimension (non-legacy) key.
    pub fn any_dimension_key() -> Self {
        Matcher::leaf(Leaf::AnyDimensionKey)
    }

    /// Matches when **all** of `matchers` match (AND). An empty set matches everything.
    pub fn all(matchers: impl IntoIterator<Item = Matcher>) -> Self {
        Matcher {
            node: Node::All(matchers.into_iter().map(|m| m.node).collect()),
        }
    }

    /// Matches when **any** of `matchers` match (OR). An empty set matches nothing.
    pub fn any_of(matchers: impl IntoIterator<Item = Matcher>) -> Self {
        Matcher {
            node: Node::Any(matchers.into_iter().map(|m| m.node).collect()),
        }
    }

    /// The conjunction of this matcher with `other` (`self AND other`).
    pub fn and(self, other: Matcher) -> Self {
        Matcher {
            node: Node::All(vec![self.node, other.node]),
        }
    }

    /// The disjunction of this matcher with `other` (`self OR other`).
    pub fn or(self, other: Matcher) -> Self {
        Matcher {
            node: Node::Any(vec![self.node, other.node]),
        }
    }

    /// The negation of this matcher (`NOT self`).
    // `not`/`and`/`or` read naturally as the boolean-combinator vocabulary here; the
    // `std::ops::Not` lookalike is intentional and not a `!self` operator.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        Matcher {
            node: Node::Not(Box::new(self.node)),
        }
    }

    /// Whether this matcher selects the given metric.
    fn matches(&self, info: &MetricInfo<'_>) -> bool {
        self.node.matches(info)
    }

    /// This matcher's default priority (higher wins), derived from its shape. Overridable per rule
    /// via [`Policy::rule_with_priority`].
    fn default_priority(&self) -> u32 {
        self.node.specificity()
    }
}

/// What a matched metric's records are subjected to by the [`Filtered`] wrapper.
///
/// `#[non_exhaustive]` (as is the [`Collapse`](Self::Collapse) variant) so future actions or fields
/// can be added without a breaking change, matching the crate's convention on
/// [`MetricInfo`](crate::MetricInfo) / [`MetricDescriptor`](crate::MetricDescriptor).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Action {
    /// Keep the metric unchanged — forward it to the inner backend verbatim.
    ///
    /// This is the same outcome as no rule matching, but as an *explicit* rule it participates in
    /// priority resolution, so a narrow `Keep` can override a broader [`Drop`](Self::Drop) or
    /// [`Collapse`](Self::Collapse): e.g. `drop(glob("send.*"))` then `keep(name("send.critical"))`
    /// filters the whole prefix but opts one metric back in.
    Keep,
    /// Drop the metric entirely — it is not forwarded to the inner backend.
    Drop,
    /// Collapse the metric across the named aggregation key: every series sharing the same name and
    /// the same remaining aggregation is merged into one (values summed, histogram buckets merged)
    /// and re-emitted with `key` removed from its aggregation. An empty `key` (`""`) collapses the
    /// keyless/bare dimension (e.g. a pre-migration `send.5`).
    #[non_exhaustive]
    Collapse { key: Arc<str> },
    /// Appends non-legacy tags to each forwarded metric name.
    /// Also merges records by original name, residual legacy tags, kind, unit, and histogram scale,
    /// emitting those aggregates at report end.
    FlattenDimensions,
}

/// One matcher-plus-action entry in a [`Policy`], with a resolution priority (higher wins).
#[derive(Clone, Debug)]
struct Rule {
    matcher: Matcher,
    action: Action,
    priority: u32,
}

/// A per-backend filtering + collapse policy, applied by [`Filtered`].
///
/// The highest-priority matching rule's [`Action`] applies (ties broken by later declaration), and
/// an unmatched metric passes through unchanged. Convenience constructors default each rule's
/// priority from its matcher shape (see the module docs); use
/// [`rule_with_priority`](Self::rule_with_priority) to pin it. Cloning a `Policy` copies its rule
/// vector (each matcher's leaf strings are `Arc`, refcount-bumped; the tree nodes are copied) —
/// cheap for typical rule counts, and a policy is normally built once and moved into a backend.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    rules: Vec<Rule>,
}

impl Policy {
    /// An empty policy: every metric passes through unchanged.
    pub fn new() -> Self {
        Policy::default()
    }

    /// Adds a rule parsed from a text expression, e.g.
    /// `"collapse worker where name ^= 'send.' and level = debug"`.
    ///
    /// The grammar is `<keep | drop | collapse KEY> [ where PREDICATE ]`, where `PREDICATE` combines
    /// `name`/tag/`agg.KEY` leaves with `and`/`or`/`not` and parentheses.
    ///
    /// This is the config-file entry point: a deployment reads rule strings from TOML/JSON and adds
    /// them here. Priority is derived from the matcher shape, as with [`rule`](Self::rule).
    pub fn rule_expr(self, expr: impl AsRef<str>) -> Result<Self, ParsePolicyError> {
        let (matcher, action) = dsl::parse_rule(expr.as_ref())?;
        Ok(self.rule(matcher, action))
    }

    /// Builds a policy from a sequence of rule expressions (one rule per item). Blank items and
    /// lines whose first non-whitespace character is `#` are skipped (so a config can carry
    /// comments). See [`rule_expr`](Self::rule_expr).
    pub fn from_exprs<I, S>(exprs: I) -> Result<Self, ParsePolicyError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut policy = Policy::new();
        for expr in exprs {
            let expr = expr.as_ref();
            // Trim only to decide blank/comment; parse the ORIGINAL string so a `ParsePolicyError`
            // position still refers to an offset in the caller's line (the lexer skips whitespace).
            let trimmed = expr.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            policy = policy.rule_expr(expr)?;
        }
        Ok(policy)
    }

    /// The number of rules in this policy.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Adds a rule pairing `matcher` with `action`, with a priority defaulted from the matcher's
    /// shape (exact name > prefix > glob > tag/aggregation-only > broad).
    pub fn rule(self, matcher: Matcher, action: Action) -> Self {
        let priority = matcher.default_priority();
        self.rule_with_priority(matcher, action, priority)
    }

    /// Adds a rule with an explicit `priority` (higher wins), overriding the shape-derived default.
    pub fn rule_with_priority(mut self, matcher: Matcher, action: Action, priority: u32) -> Self {
        self.rules.push(Rule {
            matcher,
            action,
            priority,
        });
        self
    }

    /// Adds a [`Collapse`](Action::Collapse) rule: metrics selected by `matcher` are merged across
    /// the given aggregation `key`.
    pub fn collapse(self, matcher: Matcher, key: impl Into<Arc<str>>) -> Self {
        self.rule(matcher, Action::Collapse { key: key.into() })
    }

    /// Adds a [`Drop`](Action::Drop) rule: metrics selected by `matcher` are filtered out.
    pub fn drop(self, matcher: Matcher) -> Self {
        self.rule(matcher, Action::Drop)
    }

    /// Adds a [`Keep`](Action::Keep) rule: metrics selected by `matcher` are forwarded unchanged.
    ///
    /// Useful with a higher priority than a broader `drop`/`collapse` to opt specific metrics back
    /// in (e.g. `drop(prefix("send."))` then `keep(name("send.critical"))`).
    pub fn keep(self, matcher: Matcher) -> Self {
        self.rule(matcher, Action::Keep)
    }

    /// Resolves the [`Action`] for a metric: the highest-priority matching rule wins (ties broken by
    /// later declaration), or `None` if no rule matches (the metric passes through).
    fn resolve(&self, info: &MetricInfo<'_>) -> Option<&Action> {
        let mut best: Option<(&Rule, u32)> = None;
        for rule in &self.rules {
            if !rule.matcher.matches(info) {
                continue;
            }
            // `>=` so a later-declared rule of equal priority wins (an explicit override).
            if best.is_none_or(|(_, best_prio)| rule.priority >= best_prio) {
                best = Some((rule, rule.priority));
            }
        }
        best.map(|(rule, _)| &rule.action)
    }
}

/// The *shape* of a metric that must match for two series to be merged into one collapsed group:
/// its kind, display unit, and (for histograms) fixed-point scale. Two series that share a name and
/// residual aggregation but differ in shape are **not** the same metric and must not be summed
/// together — they stay separate groups and are emitted independently, exactly as the bare backend
/// would. `scale_bits` is the histogram scale's raw `f64` bit pattern so the shape stays `Ord`/`Eq`
/// (a plain `f64` is neither); it is `0` for non-histogram kinds.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Shape {
    kind: MetricKind,
    unit: Unit,
    scale_bits: u64,
}

impl Shape {
    fn of(info: &MetricInfo<'_>, scale: f64) -> Self {
        // Scale only distinguishes histograms; pin it to 0 for every other kind so the sentinel is
        // uniform regardless of what a caller passes, matching the documented contract.
        let scale_bits = if matches!(info.kind, MetricKind::Histogram) {
            scale.to_bits()
        } else {
            0
        };
        Shape {
            kind: info.kind,
            unit: info.unit,
            scale_bits,
        }
    }
}

/// The key identifying a collapsed group: the metric name, its residual aggregation (after the
/// collapse key is removed), and its [`Shape`]. Including the shape means series of differing
/// kind/unit/scale never merge (avoiding silent data loss); ordered so flushed output is stable.
type GroupKey = (Arc<str>, Option<Arc<str>>, Shape);

/// The merged value of a collapsed group, plus the metadata needed to re-emit it.
struct Group {
    unit: Unit,
    sparsity: Sparsity,
    tags: crate::MetricTagSet,
    value: MergedValue,
}

/// The per-kind accumulator for a collapsed group.
enum MergedValue {
    Counter(u64),
    Bool {
        true_: u64,
        false_: u64,
    },
    Gauge(i64),
    /// Callback/gauge-like readings summed to a single value (matches the existing multi-callback
    /// reduction in the statsd/prometheus backends).
    Callback(f64),
    /// Element-wise sum of the source histograms' bucket arrays (all share one bucket config), plus
    /// the recorded scale.
    Histogram {
        buckets: Vec<u64>,
        scale: f64,
    },
}

/// A [`Backend`] that applies a [`Policy`] in front of an inner backend: dropping filtered metrics,
/// forwarding unmatched ones unchanged, and collapsing matched ones into a single merged series.
///
/// Collapsed groups are buffered during the report pass and flushed to the inner backend at
/// [`report_end`](Backend::report_end); unmatched metrics forward immediately (no buffering). The
/// inner backend still applies its own zero/sparsity policy to what it receives.
pub struct Filtered<B> {
    policy: Policy,
    inner: B,
    /// Collapsed groups accumulated during the current report, keyed by `(name, residual agg)`.
    /// A `BTreeMap` keeps the flushed order stable.
    groups: BTreeMap<GroupKey, Group>,
    /// Memoized policy resolution, keyed by metric *pointer identity*. The registry hands back the
    /// same `Arc` instances for a metric's name and aggregation every report (they live in the
    /// registry map and are only borrowed into `MetricInfo`), and the registered set is fixed after
    /// startup, so pointer identity is a stable, allocation-free key. `(name, aggregation)` uniquely
    /// determines a registry entry — and therefore its tags — so it fully determines the policy
    /// decision. The cached `Decision::Collapse` carries the precomputed residual aggregation, so
    /// the rule scan *and* the residual re-serialization each run once per metric ever; every later
    /// report is a pointer-keyed lookup. Persists across reports (not cleared in `report_start`).
    cache: HashMap<CacheKey, Decision>,
}

// SAFETY: the only `!Send` field is `cache`, whose keys are raw `*const u8`
// ([`CacheKey`]) used purely as opaque hash-map keys — they are never
// dereferenced. The pointers address `Arc<str>` allocations owned by the metric
// registry, which outlives every report, so nothing thread-local or unsynchronized
// is reachable through them. A `Filtered` is therefore safe to move across threads
// exactly when its inner backend is.
unsafe impl<B: Send> Send for Filtered<B> {}

/// A metric's pointer identity: the data pointers of its name and (optional) aggregation `Arc<str>`.
/// Stable across reports (the registry reuses the same allocations) and unique per registry entry,
/// so it keys the resolution cache without cloning or hashing string content.
type CacheKey = (*const u8, *const u8);

/// Builds the pointer-identity [`CacheKey`] for a metric. Uses the null pointer for a missing
/// aggregation (no real `str` allocation has a null data pointer, so it can't collide).
fn cache_key(info: &MetricInfo<'_>) -> CacheKey {
    let name = Arc::as_ptr(info.name) as *const u8;
    let agg = info
        .aggregation
        .map(|a| Arc::as_ptr(a) as *const u8)
        .unwrap_or(std::ptr::null());
    (name, agg)
}

impl<B> Filtered<B> {
    /// Wraps `inner`, applying `policy` to every metric before it reaches `inner`.
    ///
    /// Mirrors [`Toggle::new`](crate::Toggle::new): the wrapped backend comes first, its config
    /// second.
    pub fn new(inner: B, policy: Policy) -> Self {
        Filtered {
            policy,
            inner,
            groups: BTreeMap::new(),
            cache: HashMap::new(),
        }
    }

    /// A reference to the wrapped backend.
    pub fn inner(&self) -> &B {
        &self.inner
    }

    /// The residual aggregation for a metric after removing `collapse_key`, re-serialized to the
    /// canonical form. Returns `None` when nothing remains. Computed once per metric (the result is
    /// memoized in [`Decision::Collapse`]), not per record.
    fn residual_aggregation(info: &MetricInfo<'_>, collapse_key: &str) -> Option<Arc<str>> {
        let residual = info
            .aggregation_tags()
            .iter()
            .filter(|t| t.key != collapse_key)
            .map(|t| (t.key, t.value));
        crate::tags::serialize_arc(residual)
    }

    /// The residual aggregation after removing every dimension (non-legacy) key, keeping only
    /// legacy tags. Memoized like [`residual_aggregation`](Self::residual_aggregation).
    fn residual_legacy(info: &MetricInfo<'_>) -> Option<Arc<str>> {
        let residual = info
            .aggregation_tags()
            .iter()
            .filter(|t| crate::tags::is_legacy_tag_key(t.key))
            .map(|t| (t.key, t.value));
        crate::tags::serialize_arc(residual)
    }

    /// Appends non-legacy tags as `.key.value` in their aggregation-string order.
    /// The result is cached in `Decision::Flatten`.
    fn flattened_name(info: &MetricInfo<'_>) -> Arc<str> {
        let mut flat = String::from(&**info.name);
        for tag in info
            .aggregation_tags()
            .iter()
            .filter(|t| !crate::tags::is_legacy_tag_key(t.key))
        {
            flat.push('.');
            flat.push_str(tag.key);
            flat.push('.');
            flat.push_str(tag.value);
        }
        Arc::from(flat)
    }
}

impl<B: Backend> Filtered<B> {
    /// Resolves (and memoizes) the policy decision for `info`. The rule scan and residual
    /// computation run once per distinct metric identity; subsequent reports hit the cache.
    #[inline]
    fn decide(&mut self, info: &MetricInfo<'_>) -> Decision {
        let key = cache_key(info);
        if let Some(decision) = self.cache.get(&key) {
            return decision.clone();
        }
        let decision = match self.policy.resolve(info) {
            None | Some(Action::Keep) => Decision::Pass,
            Some(Action::Drop) => Decision::Drop,
            Some(Action::Collapse { key }) => Decision::Collapse {
                residual: Self::residual_aggregation(info, key),
            },
            Some(Action::FlattenDimensions) => Decision::Flatten {
                flat_name: Self::flattened_name(info),
                residual: Self::residual_legacy(info),
            },
        };
        self.cache.insert(key, decision.clone());
        decision
    }

    /// Looks up (or creates) the group entry for a collapsed record. The group key includes the
    /// metric's [`Shape`], so only series of identical kind/unit/scale merge; `init` builds the
    /// zeroed accumulator lazily, so a hit allocates nothing.
    fn group_entry(
        &mut self,
        info: &MetricInfo<'_>,
        residual: &Option<Arc<str>>,
        scale: f64,
        init: impl FnOnce() -> MergedValue,
    ) -> &mut Group {
        let key = (info.name.clone(), residual.clone(), Shape::of(info, scale));
        self.groups.entry(key).or_insert_with(|| Group {
            unit: info.unit,
            sparsity: info.sparsity,
            tags: info.tags.to_set(),
            value: init(),
        })
    }
}

/// The resolution outcome for one record. Cloned out of the memoization cache per record (cheap: at
/// most `Arc` refcount bumps).
#[derive(Clone)]
enum Decision {
    /// No rule matched — forward to the inner backend unchanged.
    Pass,
    /// A `Drop` rule matched — skip.
    Drop,
    /// A `Collapse` rule matched — buffer under this precomputed residual aggregation (the source
    /// aggregation with the collapse key removed).
    Collapse { residual: Option<Arc<str>> },
    /// Forwards with a suffixed name and buffers by original name, residual tags, and metric shape.
    Flatten {
        flat_name: Arc<str>,
        residual: Option<Arc<str>>,
    },
}

/// The forwarded [`MetricInfo`] for a flattened record: the dimension-suffixed name carrying only
/// the residual legacy aggregation, all other metadata unchanged.
fn flat_info<'a>(
    info: &MetricInfo<'a>,
    flat_name: &'a Arc<str>,
    residual: &'a Option<Arc<str>>,
) -> MetricInfo<'a> {
    let mut flat = MetricInfo::new(flat_name, residual.as_ref(), info.unit, info.kind);
    flat.sparsity = info.sparsity;
    flat.tags = info.tags;
    flat
}

impl<B: Backend> Backend for Filtered<B> {
    fn report_start(&mut self, options: &ReportOptions) {
        // Clear retains capacity — the wrapper is long-lived and reused across reports.
        self.groups.clear();
        self.inner.report_start(options);
    }

    fn record_counter(&mut self, info: &MetricInfo<'_>, value: u64) {
        let decision = self.decide(info);
        if let Decision::Flatten {
            flat_name,
            residual,
        } = &decision
        {
            self.inner
                .record_counter(&flat_info(info, flat_name, residual), value);
        }
        match decision {
            Decision::Pass => self.inner.record_counter(info, value),
            Decision::Drop => {}
            Decision::Collapse { residual } | Decision::Flatten { residual, .. } => {
                // scale is irrelevant for non-histograms; 1.0 keeps the Shape uniform.
                let group = self.group_entry(info, &residual, 1.0, || MergedValue::Counter(0));
                if let MergedValue::Counter(total) = &mut group.value {
                    // Saturate rather than overflow (debug panic / release wrap) when summing many
                    // per-worker series.
                    *total = total.saturating_add(value);
                }
            }
        }
    }

    fn record_gauge(&mut self, info: &MetricInfo<'_>, value: i64) {
        let decision = self.decide(info);
        if let Decision::Flatten {
            flat_name,
            residual,
        } = &decision
        {
            self.inner
                .record_gauge(&flat_info(info, flat_name, residual), value);
        }
        match decision {
            Decision::Pass => self.inner.record_gauge(info, value),
            Decision::Drop => {}
            Decision::Collapse { residual } | Decision::Flatten { residual, .. } => {
                let group = self.group_entry(info, &residual, 1.0, || MergedValue::Gauge(0));
                if let MergedValue::Gauge(total) = &mut group.value {
                    // Saturate: summing e.g. per-worker timestamp gauges can exceed i64.
                    *total = total.saturating_add(value);
                }
            }
        }
    }

    fn record_bool(&mut self, info: &MetricInfo<'_>, true_count: u64, false_count: u64) {
        let decision = self.decide(info);
        if let Decision::Flatten {
            flat_name,
            residual,
        } = &decision
        {
            self.inner.record_bool(
                &flat_info(info, flat_name, residual),
                true_count,
                false_count,
            );
        }
        match decision {
            Decision::Pass => self.inner.record_bool(info, true_count, false_count),
            Decision::Drop => {}
            Decision::Collapse { residual } | Decision::Flatten { residual, .. } => {
                let group = self.group_entry(info, &residual, 1.0, || MergedValue::Bool {
                    true_: 0,
                    false_: 0,
                });
                if let MergedValue::Bool { true_, false_ } = &mut group.value {
                    *true_ = true_.saturating_add(true_count);
                    *false_ = false_.saturating_add(false_count);
                }
            }
        }
    }

    fn record_histogram(&mut self, info: &MetricInfo<'_>, hist: Histogram<'_>) {
        let decision = self.decide(info);
        if let Decision::Flatten {
            flat_name,
            residual,
        } = &decision
        {
            self.inner
                .record_histogram(&flat_info(info, flat_name, residual), hist);
        }
        match decision {
            Decision::Pass => self.inner.record_histogram(info, hist),
            Decision::Drop => {}
            Decision::Collapse { residual } | Decision::Flatten { residual, .. } => {
                let scale = hist.scale();
                // The Shape includes `scale`, so histograms of differing scale land in separate
                // groups; every record in one group therefore shares this scale and the same
                // `CONFIG` bucket layout, so the arrays sum element-wise losslessly. `init` runs
                // only on a miss (no per-record Vec alloc on a hit).
                let group = self.group_entry(info, &residual, scale, || MergedValue::Histogram {
                    buckets: vec![0; hist.raw_buckets().len()],
                    scale,
                });
                if let MergedValue::Histogram { buckets, .. } = &mut group.value {
                    for (slot, add) in buckets.iter_mut().zip(hist.raw_buckets()) {
                        *slot = slot.saturating_add(*add);
                    }
                }
            }
        }
    }

    fn record_callback(&mut self, info: &MetricInfo<'_>, values: &[&dyn CallbackValue]) {
        let decision = self.decide(info);
        if let Decision::Flatten {
            flat_name,
            residual,
        } = &decision
        {
            self.inner
                .record_callback(&flat_info(info, flat_name, residual), values);
        }
        match decision {
            Decision::Pass => self.inner.record_callback(info, values),
            Decision::Drop => {}
            Decision::Collapse { residual } | Decision::Flatten { residual, .. } => {
                let sum: f64 = values.iter().map(|v| v.as_f64()).sum();
                let group = self.group_entry(info, &residual, 1.0, || MergedValue::Callback(0.0));
                if let MergedValue::Callback(total) = &mut group.value {
                    *total += sum;
                }
            }
        }
    }

    fn report_end(&mut self) {
        // Flush every collapsed group to the inner backend in stable (name, residual, shape) order,
        // then end the inner report. Disjoint field borrows let us read `groups` while mutating
        // `inner`.
        let Filtered { inner, groups, .. } = self;
        for ((name, aggregation, _shape), group) in groups.iter() {
            let mut info = MetricInfo::new(
                name,
                aggregation.as_ref(),
                group.unit,
                merged_kind(&group.value),
            );
            info.sparsity = group.sparsity;
            let tags_view = crate::MetricTags::new(&group.tags);
            info.tags = tags_view;
            match &group.value {
                MergedValue::Counter(v) => inner.record_counter(&info, *v),
                MergedValue::Gauge(v) => inner.record_gauge(&info, *v),
                MergedValue::Bool { true_, false_ } => inner.record_bool(&info, *true_, *false_),
                MergedValue::Callback(sum) => {
                    let values: [&dyn CallbackValue; 1] = [sum];
                    inner.record_callback(&info, &values);
                }
                MergedValue::Histogram { buckets, scale } => {
                    let view = Histogram::new(buckets, &crate::summary::CONFIG, group.unit, *scale);
                    inner.record_histogram(&info, view);
                }
            }
        }
        self.inner.report_end();
    }
}

/// The [`MetricKind`] a merged group re-emits as.
fn merged_kind(value: &MergedValue) -> MetricKind {
    match value {
        MergedValue::Counter(_) => MetricKind::Counter,
        MergedValue::Bool { .. } => MetricKind::BoolCounter,
        MergedValue::Gauge(_) => MetricKind::Gauge,
        MergedValue::Callback(_) => MetricKind::CallbackScalar,
        MergedValue::Histogram { .. } => MetricKind::Histogram,
    }
}

/// Matches `text` against a `*`-glob `pattern`. `*` matches any run (including empty); all other
/// characters match literally. There is no `?` or character-class support — just `*`.
fn glob_match(pattern: &str, text: &str) -> bool {
    // Classic linear-time backtracking glob: advance both, remembering the last `*` position so a
    // mismatch can retry by having `*` absorb one more character.
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    // Consume any trailing `*`s in the pattern.
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::Registry;

    /// A backend that records every emitted `(name, aggregation, kind, value/summary)` for asserting
    /// what reached the inner backend after the policy ran.
    #[derive(Default)]
    struct Capture {
        counters: Vec<(String, Option<String>, u64)>,
        bools: Vec<(String, Option<String>, u64, u64)>,
        gauges: Vec<(String, Option<String>, i64)>,
        callbacks: Vec<(String, Option<String>, f64)>,
        histograms: Vec<(String, Option<String>, u64)>,
    }

    impl Capture {
        fn agg(info: &MetricInfo<'_>) -> Option<String> {
            info.aggregation.map(|a| a.to_string())
        }
    }

    impl Backend for Capture {
        fn record_counter(&mut self, info: &MetricInfo<'_>, value: u64) {
            self.counters
                .push((info.name.to_string(), Self::agg(info), value));
        }
        fn record_gauge(&mut self, info: &MetricInfo<'_>, value: i64) {
            self.gauges
                .push((info.name.to_string(), Self::agg(info), value));
        }
        fn record_bool(&mut self, info: &MetricInfo<'_>, t: u64, f: u64) {
            self.bools
                .push((info.name.to_string(), Self::agg(info), t, f));
        }
        fn record_histogram(&mut self, info: &MetricInfo<'_>, hist: Histogram<'_>) {
            self.histograms
                .push((info.name.to_string(), Self::agg(info), hist.count()));
        }
        fn record_callback(&mut self, info: &MetricInfo<'_>, values: &[&dyn CallbackValue]) {
            let sum = values.iter().map(|v| v.as_f64()).sum();
            self.callbacks
                .push((info.name.to_string(), Self::agg(info), sum));
        }
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("send.*", "send.5"));
        assert!(glob_match("*.lost", "send.lost"));
        assert!(glob_match("tx.*.frame", "tx.acked.frame"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("send.*", "recv.5"));
        assert!(!glob_match("*.lost", "send.found"));
        assert!(!glob_match("exact", "exactly"));
        assert!(glob_match("a*b*c", "axxbyyc"));
    }

    /// Collapsing `worker` merges every per-worker counter series of a name into one (values
    /// summed, `worker` dropped from the aggregation), while a low-cardinality `variant` metric and
    /// an unmatched metric pass through untouched.
    #[test]
    fn collapse_worker_merges_counters_and_leaves_others() {
        let registry = Registry::new();
        // Three per-worker series of one loss counter.
        for w in 0..3 {
            registry
                .metric("send.lost")
                .aggregation(format!("worker|send.{w}"))
                .tag("level", "debug")
                .counter()
                .increment(w + 1); // 1 + 2 + 3 = 6
        }
        // A low-cardinality variant metric (no worker key) and an unruled metric.
        registry
            .metric("rx.ecn")
            .aggregation("Variant|ect0")
            .counter()
            .increment(10);
        registry
            .register_counter("rx.data".into(), None)
            .increment(42);

        // Collapse the `worker` aggregation key on everything tagged level=debug.
        let policy = Policy::new().collapse(Matcher::tag("level", "debug"), "worker");
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);

        let cap = backend.inner();
        // The three worker series collapsed to a single `send.lost` line summing to 6, with `worker`
        // removed (no residual aggregation remains since it was the only key).
        let lost: Vec<_> = cap
            .counters
            .iter()
            .filter(|(n, _, _)| n == "send.lost")
            .collect();
        assert_eq!(lost, vec![&("send.lost".to_string(), None, 6)]);
        // The variant and unruled metrics passed through verbatim.
        assert!(cap.counters.contains(&(
            "rx.ecn".to_string(),
            Some("Variant|ect0".to_string()),
            10
        )));
        assert!(cap.counters.contains(&("rx.data".to_string(), None, 42)));
    }

    #[test]
    fn flatten_dimensions_emits_suffixed_and_bare_and_keeps_legacy() {
        let registry = Registry::new();
        for (op, result, v) in [("get", "ok", 1), ("get", "err", 2), ("put", "ok", 4)] {
            registry
                .metric("api.calls")
                .aggregation(format!("operation|{op},result|{result}"))
                .counter()
                .increment(v);
        }
        registry
            .metric("rx.ecn")
            .aggregation("Variant|ect0")
            .counter()
            .increment(10);
        registry
            .register_counter("rx.data".into(), None)
            .increment(42);

        let policy = Policy::new().rule(Matcher::any_dimension_key(), Action::FlattenDimensions);
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);

        let cap = backend.inner();
        for (suffixed, v) in [
            ("api.calls.operation.get.result.ok", 1),
            ("api.calls.operation.get.result.err", 2),
            ("api.calls.operation.put.result.ok", 4),
        ] {
            assert!(
                cap.counters.contains(&(suffixed.to_string(), None, v)),
                "missing {suffixed}: {:?}",
                cap.counters
            );
        }
        let bare: Vec<_> = cap
            .counters
            .iter()
            .filter(|(n, _, _)| n == "api.calls")
            .collect();
        assert_eq!(bare, vec![&("api.calls".to_string(), None, 7)]);
        assert!(cap.counters.contains(&(
            "rx.ecn".to_string(),
            Some("Variant|ect0".to_string()),
            10
        )));
        assert!(cap.counters.contains(&("rx.data".to_string(), None, 42)));
    }

    /// A `Drop` rule removes a metric from the backend entirely.
    #[test]
    fn drop_filters_metric_out() {
        let registry = Registry::new();
        registry
            .register_counter("tx.acked.frame.queue_data".into(), None)
            .increment(5);
        registry
            .register_counter("rx.data".into(), None)
            .increment(7);

        let policy = Policy::new().drop(Matcher::prefix("tx.acked.frame."));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);

        let cap = backend.inner();
        assert!(!cap
            .counters
            .iter()
            .any(|(n, _, _)| n.starts_with("tx.acked.frame.")));
        assert!(cap.counters.contains(&("rx.data".to_string(), None, 7)));
    }

    /// Collapse merges every kind: bool sides sum, gauge/callback values sum, and histogram buckets
    /// merge into one distribution whose count is the total of the per-worker samples.
    #[test]
    fn collapse_merges_all_kinds() {
        let registry = Registry::new();
        for w in 0..2 {
            let v = format!("worker|send.{w}");
            registry
                .metric("connect")
                .aggregation(v.clone())
                .bool()
                .record(true);
            registry
                .metric("send.cwnd")
                .aggregation(v.clone())
                .summary(Unit::Byte)
                .record_value(100 * (w + 1));
            registry
                .metric("send.context.count")
                .aggregation(v.clone())
                .sparse()
                .list_callback(Unit::Count, move || (w + 1) as i64);
        }

        let policy = Policy::new().collapse(Matcher::agg_key("worker"), "worker");
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let cap = backend.inner();

        // Two `connect` true records collapsed into one bool with true=2.
        assert_eq!(cap.bools, vec![("connect".to_string(), None, 2, 0)]);
        // Two callback readings (1 + 2) summed into one gauge-like value.
        assert_eq!(
            cap.callbacks,
            vec![("send.context.count".to_string(), None, 3.0)]
        );
        // Two histogram samples merged into one distribution of count 2.
        assert_eq!(cap.histograms, vec![("send.cwnd".to_string(), None, 2)]);
    }

    /// Most-specific match wins: an exact-name `keep` (no rule) overrides a broad prefix `collapse`.
    /// Modeled by a prefix collapse plus an exact-name drop that shadows one metric.
    #[test]
    fn most_specific_rule_wins() {
        let registry = Registry::new();
        registry
            .metric("send.a")
            .aggregation("worker|send.0")
            .counter()
            .increment(1);
        registry
            .metric("send.b")
            .aggregation("worker|send.0")
            .counter()
            .increment(2);

        // Broad prefix collapses worker; a more-specific exact-name rule drops `send.b`.
        let policy = Policy::new()
            .collapse(Matcher::prefix("send."), "worker")
            .drop(Matcher::name("send.b"));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let cap = backend.inner();

        // send.a collapsed (worker removed); send.b dropped by the more specific exact rule.
        assert_eq!(cap.counters, vec![("send.a".to_string(), None, 1)]);
    }

    /// Without a policy the same registry emits one line per worker; with the collapse policy it
    /// emits one — proving the cardinality reduction.
    #[test]
    fn collapse_reduces_line_count() {
        let build = || {
            let registry = Registry::new();
            for w in 0..8 {
                registry
                    .metric("send.lost")
                    .aggregation(format!("worker|send.{w}"))
                    .counter()
                    .increment(1);
            }
            registry
        };

        // Bare backend: 8 distinct per-worker lines.
        let registry = build();
        let mut bare = Capture::default();
        registry.report_with(&ReportOptions::new(false), &mut bare);
        assert_eq!(bare.counters.len(), 8);

        // Collapsed: a single merged line summing to 8.
        let registry = build();
        let policy = Policy::new().collapse(Matcher::name("send.lost"), "worker");
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        assert_eq!(
            backend.inner().counters,
            vec![("send.lost".to_string(), None, 8)]
        );
    }

    /// Compound matchers: `send.* OR recv.*` selects both prefixes; a `NOT` excludes a subset; and
    /// an `all(...)` (name AND tag) only matches when both hold.
    #[test]
    fn compound_and_or_not() {
        let registry = Registry::new();
        registry
            .register_counter("send.x".into(), None)
            .increment(1);
        registry
            .register_counter("recv.x".into(), None)
            .increment(1);
        registry
            .register_counter("other.x".into(), None)
            .increment(1);

        // OR: drop send.* or recv.*, keep other.*.
        let policy = Policy::new().drop(Matcher::prefix("send.").or(Matcher::prefix("recv.")));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let names: Vec<_> = backend
            .inner()
            .counters
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["other.x"]);

        // NOT: drop everything that is NOT rx.keep.
        let registry = Registry::new();
        registry
            .register_counter("rx.keep".into(), None)
            .increment(1);
        registry
            .register_counter("rx.toss".into(), None)
            .increment(1);
        let policy = Policy::new().drop(Matcher::name("rx.keep").not());
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let names: Vec<_> = backend
            .inner()
            .counters
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["rx.keep"]);

        // AND: only send.* metrics also tagged level=debug are dropped.
        let registry = Registry::new();
        registry
            .metric("send.a")
            .tag("level", "debug")
            .counter()
            .increment(1);
        registry
            .metric("send.b")
            .tag("level", "info")
            .counter()
            .increment(1);
        let policy = Policy::new().drop(Matcher::all([
            Matcher::prefix("send."),
            Matcher::tag("level", "debug"),
        ]));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let names: Vec<_> = backend
            .inner()
            .counters
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["send.b"]);
    }

    /// Tag value matching: `any_of` (`level = debug|info`), `present` (`level=*`), and value glob.
    #[test]
    fn tag_value_matchers() {
        let build = || {
            let registry = Registry::new();
            registry
                .metric("a")
                .tag("level", "debug")
                .counter()
                .increment(1);
            registry
                .metric("b")
                .tag("level", "info")
                .counter()
                .increment(1);
            registry
                .metric("c")
                .tag("level", "warn")
                .counter()
                .increment(1);
            registry.metric("d").counter().increment(1); // untagged
            registry
        };

        // any_of debug|info drops a and b.
        let registry = build();
        let policy = Policy::new().drop(Matcher::tag_any_of("level", ["debug", "info"]));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let names: Vec<_> = backend
            .inner()
            .counters
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["c", "d"]);

        // present (level=*) drops every tagged metric, leaving the untagged one.
        let registry = build();
        let policy = Policy::new().drop(Matcher::tag_present("level"));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let names: Vec<_> = backend
            .inner()
            .counters
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["d"]);

        // value glob `w*` drops only warn.
        let registry = build();
        let policy = Policy::new().drop(Matcher::tag_glob("level", "w*"));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let names: Vec<_> = backend
            .inner()
            .counters
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b", "d"]);
    }

    /// An explicit priority overrides the shape-derived default: a broad glob rule can be made to
    /// win over an exact-name rule when given a higher priority.
    #[test]
    fn explicit_priority_overrides_shape() {
        let registry = Registry::new();
        registry
            .register_counter("send.x".into(), None)
            .increment(1);

        // Exact name would normally win, but the glob rule is pinned to a higher priority, so the
        // glob's Drop applies instead of the exact rule's Collapse.
        let policy = Policy::new()
            .collapse(Matcher::name("send.x"), "worker") // default priority ~high (exact)
            .rule_with_priority(Matcher::glob("send.*"), Action::Drop, u32::MAX);
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        // Dropped by the higher-priority glob rule.
        assert!(backend.inner().counters.is_empty());
    }

    /// A narrow `Keep` opts a metric back in over a broad glob `Drop`: the exact-name `Keep` is
    /// more specific, so it wins, while the rest of the prefix stays dropped.
    #[test]
    fn keep_opts_back_in_over_glob_drop() {
        let registry = Registry::new();
        registry
            .register_counter("send.a".into(), None)
            .increment(1);
        registry
            .register_counter("send.critical".into(), None)
            .increment(9);
        registry
            .register_counter("send.b".into(), None)
            .increment(1);

        // Drop the whole send.* prefix, but keep send.critical (exact name is more specific).
        let policy = Policy::new()
            .drop(Matcher::glob("send.*"))
            .keep(Matcher::name("send.critical"));
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);

        assert_eq!(
            backend.inner().counters,
            vec![("send.critical".to_string(), None, 9)]
        );
    }

    /// Resolution is memoized: the same metric identity resolves once and is served from the cache
    /// on subsequent reports, and the cached decision produces identical output.
    #[test]
    fn resolution_is_memoized_across_reports() {
        let registry = Registry::new();
        let c = registry
            .metric("send.lost")
            .aggregation("worker|send.0")
            .counter();

        let policy = Policy::new().collapse(Matcher::name("send.lost"), "worker");
        let mut backend = Filtered::new(Capture::default(), policy);

        c.increment(3);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        // One cache entry after the first report.
        assert_eq!(backend.cache.len(), 1);

        c.increment(4);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        // Still one entry (served from cache, not re-inserted), and the collapse still applied.
        assert_eq!(backend.cache.len(), 1);
        assert_eq!(
            backend.inner().counters,
            vec![
                ("send.lost".to_string(), None, 3),
                ("send.lost".to_string(), None, 4),
            ]
        );
    }

    /// Two series that share a name and collapse to the same residual aggregation but are of
    /// DIFFERENT kinds must NOT be merged into one group (which silently dropped one of them before
    /// the `Shape` was added to the group key). Each is emitted independently, exactly as the bare
    /// backend would — no data loss.
    #[test]
    fn mismatched_kinds_do_not_merge_or_drop() {
        let registry = Registry::new();
        // Same name "m", same collapse key `worker`, so both would collapse to residual None — but
        // one is a counter and one is a gauge.
        registry
            .metric("m")
            .aggregation("worker|send.0")
            .counter()
            .increment(5);
        registry
            .metric("m")
            .aggregation("worker|send.1")
            .gauge(Unit::Count)
            .set(7);

        let policy = Policy::new().collapse(Matcher::name("m"), "worker");
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);
        let cap = backend.inner();

        // Both survive as their own kind — neither is silently dropped.
        assert_eq!(cap.counters, vec![("m".to_string(), None, 5)]);
        assert_eq!(cap.gauges, vec![("m".to_string(), None, 7)]);
    }

    /// Two histogram series of the same name/residual but DIFFERENT scale must not merge (mixing
    /// fixed-point resolutions would corrupt the de-scaled values); they stay separate groups.
    #[test]
    fn mismatched_scales_do_not_merge() {
        let registry = Registry::new();
        registry
            .metric("h")
            .aggregation("worker|send.0")
            .summary(Unit::Count) // scale 1.0
            .record_value(100);
        registry
            .metric("h")
            .aggregation("worker|send.1")
            .scale(1e6)
            .summary(Unit::Count) // scale 1e6
            .record_f64(0.25);

        let policy = Policy::new().collapse(Matcher::name("h"), "worker");
        let mut backend = Filtered::new(Capture::default(), policy);
        registry.report_with(&ReportOptions::new(false), &mut backend);

        // Two separate histogram groups (differing scale), each with its own single sample — not one
        // corrupted merged distribution.
        assert_eq!(backend.inner().histograms.len(), 2);
        assert!(backend
            .inner()
            .histograms
            .iter()
            .all(|(n, _, count)| n == "h" && *count == 1));
    }

    /// Collapsing many counter series whose sum exceeds `u64::MAX` saturates rather than wrapping;
    /// the analogous gauge path uses `saturating_add` too (a summed timestamp gauge can exceed i64).
    #[test]
    fn collapse_sum_saturates_on_overflow() {
        let registry = Registry::new();
        for w in 0..2 {
            registry
                .metric("g")
                .aggregation(format!("worker|send.{w}"))
                .gauge(Unit::Count)
                .set(i64::MAX);
        }
        let policy = Policy::new().collapse(Matcher::name("g"), "worker");
        let mut backend = Filtered::new(Capture::default(), policy);
        // Must not panic (debug) or wrap; saturates at i64::MAX.
        registry.report_with(&ReportOptions::new(false), &mut backend);
        assert_eq!(
            backend.inner().gauges,
            vec![("g".to_string(), None, i64::MAX)]
        );
    }
}
