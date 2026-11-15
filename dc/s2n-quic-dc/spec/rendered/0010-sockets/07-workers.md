## Busy-poll workers {#workers}

A busy-poll worker is a thread that polls its tasks in a loop, in order of priority, and sleeps only while it has no tasks. A watchdog checks each worker's progress, and stops the process when a worker's cycle doesn't complete within the configured timeout.

### DC-0010-043 {#DC-0010-043}

A busy-poll worker MUST poll each of its tasks in order of priority, highest first, in every cycle.

### DC-0010-044 {#DC-0010-044}

A busy-poll worker with no tasks MUST sleep until a task is given to it.

### DC-0010-045 {#DC-0010-045}

A busy-poll worker SHOULD yield its processor after each batch of cycles, so that other threads on it run.

### DC-0010-046 {#DC-0010-046}

A busy-poll worker MUST stop once every handle that can give it tasks is dropped.

### DC-0010-047 {#DC-0010-047}

A watchdog MUST stop the process when a worker's cycle doesn't complete within the configured timeout, after reporting the worker and the task it is stuck in.

### DC-0010-048 {#DC-0010-048}

The watchdog MUST NOT treat a worker that sleeps for lack of tasks as stuck.

### DC-0010-049 {#DC-0010-049}

A worker task that fails with an unrecovered fault MUST stop the process.
