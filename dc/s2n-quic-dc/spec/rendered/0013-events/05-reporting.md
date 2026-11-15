## Reporting metrics {#reporting}

A report reads every registered metric, resets what it read, and passes each value with its name, variant, kind and unit to the exporters. An exporter writes the values in one format, such as a text line, a datagram pushed to a collector, a page that a collector scrapes, a structured telemetry protocol or a columnar file. A metric with no recorded value in an interval reports as a zero, which the report's setting emits or drops.

### DC-0013-030 {#DC-0013-030}

A report MUST read each metric's values recorded since the previous report and reset them, so that each recorded value is reported exactly once.

### DC-0013-031 {#DC-0013-031}

A report MUST visit the registered metrics in the same order every time.

### DC-0013-032 {#DC-0013-032}

A report MUST pass one snapshot to every exporter it feeds, so that each exporter receives the same values.

### DC-0013-033 {#DC-0013-033}

A report MUST pass each exporter the value of each metric as a number rather than as text, with its name, its variant, its kind and its unit.

### DC-0013-034 {#DC-0013-034}

Whether a report emits a metric with no recorded value as a zero MUST follow the report's setting, unless the metric was registered to always emit a zero or to always drop one.

### DC-0013-035 {#DC-0013-035}

An application MAY turn an exporter off and on while the endpoint runs.

### DC-0013-036 {#DC-0013-036}

A metadata tag attached to a metric at registration MUST NOT add to the dimensions an exporter emits, unless that exporter chooses to emit it.

### DC-0013-037 {#DC-0013-037}

The system MAY fold recorded values into the aggregate between reports, as long as a later report still includes every value recorded.

### DC-0013-038 {#DC-0013-038}

A registry that is closed MUST stop reporting its metrics.
