## Executor and workers {#executor}

An executor bundles a way to start tasks with a clock. It has a fixed number of workers, numbered from zero. A component starts a task either on any worker or on a chosen worker, and a task started on a chosen worker runs on that worker for its whole life. Several executors meet this interface: one whose workers poll without sleeping, one whose workers each run a single-threaded event loop, a simulation that runs every worker on one thread in a deterministic order, and an inspector that holds the tasks a configuration starts without running them.

### DC-0012-001 {#DC-0012-001}

An executor MUST report a fixed number of workers, which stays the same for the executor's whole life.

### DC-0012-002 {#DC-0012-002}

A task started on a chosen worker MUST run on that worker until it completes.

### DC-0012-003 {#DC-0012-003}

An executor MUST let a component create a task on a chosen worker, so that a task whose state can't move between threads is never built on another thread.

### DC-0012-004 {#DC-0012-004}

An executor MUST record the name and the worker of each task it starts for a pipeline stage, so that a tool can show which tasks run on which worker.

### DC-0012-005 {#DC-0012-005}

A worker of an event-loop executor MUST keep running the tasks started on it after every handle to the executor is dropped.

### DC-0012-006 {#DC-0012-006}

An executor MUST NOT silently discard a task started on a worker that has stopped.

### DC-0012-007 {#DC-0012-007}

A simulation executor MUST run its tasks on one thread, in an order set by its seed and its inputs alone.

### DC-0012-008 {#DC-0012-008}

An inspector executor MUST hold each task it is given without running it.

### DC-0012-009 {#DC-0012-009}

An inspector executor MUST NOT drop a task it recorded while the inspector exists.

### DC-0012-010 {#DC-0012-010}

An inspector executor MUST report at least one worker.
