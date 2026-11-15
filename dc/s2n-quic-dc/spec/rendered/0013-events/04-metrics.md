## Metrics {#metrics}

A metric has a name, an optional variant that splits it into parts sharing that name, a kind and a unit. A counter adds counts. A gauge holds the latest value. A boolean counter counts true outcomes and false outcomes apart. A summary records a distribution of values in buckets. Each metric is registered once, before any value is recorded to it, and recording is cheap enough for the data path.

### DC-0013-021 {#DC-0013-021}

Each metric MUST be registered with a name, a kind, a unit and an optional variant before any value is recorded to it.

### DC-0013-022 {#DC-0013-022}

The kind of each metric MUST be one of a counter, a gauge, a boolean counter and a summary.

### DC-0013-023 {#DC-0013-023}

The unit of each metric MUST be one of count, microseconds, bytes, seconds and percent.

### DC-0013-024 {#DC-0013-024}

A summary MUST place each value it records in buckets whose relative error is the same at every magnitude.

### DC-0013-025 {#DC-0013-025}

Recording a value to a metric MUST NOT wait for a lock that another processor holds, except for a single counter increment larger than the per-processor record holds.

### DC-0013-026 {#DC-0013-026}

The counters registered for the variants of one enumerated outcome MUST share one metric name and differ by variant, with one variant that counts each value the enumeration doesn't list.

### DC-0013-027 {#DC-0013-027}

The name of a counter that counts a failure SHOULD mark it as a failure, so that a reader tells it apart from a count of successes.

### DC-0013-028 {#DC-0013-028}

A handle to the registry MAY add a prefix to the name of each metric registered through it, leaving the variant as it is.

### DC-0013-029 {#DC-0013-029}

The system MAY report metrics of the scheduler that runs the endpoint's tasks, such as the depth of its queues, sampled at an interval the application sets.
