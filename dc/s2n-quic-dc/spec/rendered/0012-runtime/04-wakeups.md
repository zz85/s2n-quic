## Task wakeups {#wakeups}

A task that serves many sources registers a wake source for each of them, by an identifier. Waking a source records its identifier in a shared ready set and wakes the serving task. A worker that sleeps between bursts of work records whether it is sleeping, has work pending or is working, so that a wake reaches its thread only when it sleeps. A deferred wake carries a waker to the point where a lock is released, and wakes it there.

### DC-0012-030 {#DC-0012-030}

Waking a registered source MUST record the source's identifier in the serving task's ready set and wake the serving task.

### DC-0012-031 {#DC-0012-031}

Draining a ready set MUST return each woken identifier once, however many times it was woken since the last drain.

### DC-0012-032 {#DC-0012-032}

A wake of a worker MUST wake the worker's thread only where the worker is sleeping.

### DC-0012-033 {#DC-0012-033}

A forced wake of a worker MUST wake the worker's thread whatever the worker's state.

### DC-0012-034 {#DC-0012-034}

A component MUST NOT wake a task while it holds a lock that the woken task takes.

### DC-0012-035 {#DC-0012-035}

A deferred wake that is dropped before it is delivered MUST wake its task.
