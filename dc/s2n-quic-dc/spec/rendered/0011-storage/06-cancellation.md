## Cancellation {#cancellation}

Submission has two phases: a reservation acquires the operation's credit, and the submit hands the buffer over and enqueues the operation. Dropping a reservation, or abandoning the wait for one, cancels the operation. A producer can wait for a reservation before it starts the work that fills the buffer, which bounds its fan-out by the device's capacity.

### DC-0011-031 {#DC-0011-031}

When a submitter drops a reservation, or abandons its wait for credit, the device MUST return any credit held for the operation.

### DC-0011-032 {#DC-0011-032}

The device MUST NOT issue any part of an operation whose reservation was dropped before submission.

### DC-0011-033 {#DC-0011-033}

The device MUST NOT stop an operation once the backend has started to run it, even when its submitter stops waiting.

### DC-0011-034 {#DC-0011-034}

The backend MAY skip an operation whose submitter stopped waiting for its completion before the operation started to run, provided it returns the operation's credit.

### DC-0011-035 {#DC-0011-035}

The backend MUST NOT skip an operation submitted without a request for its completion.
