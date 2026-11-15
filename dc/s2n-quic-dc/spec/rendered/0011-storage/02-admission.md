## Admission by credit {#admission}

Each device holds one or two credit pools whose capacity is the device's queue depth. An operation's cost, in credits, is taken from a pool before the operation enters the device's execution pipeline and is returned when the operation completes. A submitter that finds the pool short of credit waits without holding a thread or an execution slot. Each operation carries a priority tier. A device's configuration also holds a rate ceiling, in operations or in bytes per second, which this version stores and doesn't yet apply to admission.

### DC-0011-005 {#DC-0011-005}

The device MUST acquire an operation's whole cost in credit from its pool before the operation enters the execution pipeline.

### DC-0011-006 {#DC-0011-006}

A submitter waiting for credit MUST NOT hold a thread or an execution slot while it waits.

### DC-0011-007 {#DC-0011-007}

The device MUST grant an operation its whole cost in one grant, and never a part of it.

### DC-0011-008 {#DC-0011-008}

The device MUST NOT create execution lanes or threads in response to load after it registers.

### DC-0011-009 {#DC-0011-009}

The device MUST grant credit to the waiting submitters of one priority tier in the order in which they began to wait.

### DC-0011-010 {#DC-0011-010}

The device SHOULD grant credit to a waiting submitter of a higher priority tier before a waiting submitter of a lower tier.

### DC-0011-011 {#DC-0011-011}

The device MUST return an operation's credit to the pool it came from exactly once, whether the operation succeeds, fails or is cancelled.

### DC-0011-012 {#DC-0011-012}

The device MUST keep its configured rate ceiling, in operations or in bytes per second, with its configuration.
