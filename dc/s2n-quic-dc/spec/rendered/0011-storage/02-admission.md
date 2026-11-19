## Admission by credit {#admission}

Each device holds one or two credit pools. A pool's capacity starts at the capacity the device's configuration sets for it, the device's queue depth, and changes at run time from the device's answers. A would-block answer, such as EAGAIN, says that the device can't take an operation now; it halves the capacity of the pool the operation drew from, and the would-block answers to operations that pool granted before that decrease don't lower it again. A clean completion is the completion of an operation that the device took without a would-block answer and that returns its credit while a submitter waits for credit from the same pool. A pool's first run of clean completions begins when its device is created, and a new run begins at each would-block answer to an operation that drew from the pool, at each raise of the pool's capacity and at the end of the run before it. A run ends at the clean completion that brings the summed cost of the run's clean completions to at least the capacity the pool had when the run began, and that completion raises the capacity by its operation's cost. The capacity the pool had when a run began is the capacity as the event that began the run leaves it, after any decrease or raise that event makes, so a run that begins at a raise from 8 to 12 ends at the clean completion that brings its summed cost to at least 12, and a run that begins at a would-block answer that halves the capacity from 8 to 4 ends at the one that brings it to at least 4. A run's completion time is the longest time from the pool's grant of an operation's credit to that operation's completion among the operations of the run's clean completions, and a pool's last decrease is the latest lowering of its capacity, or the device's creation where the device hasn't lowered it yet. A run whose clean completions are all of operations that the pool granted after its last decrease, and whose completion time is at least twice the shortest completion time above zero among the pool's earlier runs of that kind since that decrease, shows the device's completion latency rising: the completion that ends that run halves the capacity, as a would-block answer does, instead of raising it. A decrease never takes the capacity below the cost of an operation that waits for the pool's credit, and an operation whose cost is higher than the capacity raises the capacity to its cost as it begins to wait. A device's configuration doesn't set an upper bound on a pool's capacity: only the device's answers stop the capacity from rising. Each change of the capacity changes the pool's free credit by the same amount, so a lower capacity takes its difference back from the credit that completing operations return, and never from an operation that holds credit. Where this RFC says a pool's capacity, it means its current capacity, and the configured capacity is the value it starts at. An operation's cost, in credits, is taken from a pool before the operation enters the device's execution pipeline and is returned when the operation completes. A submitter that finds the pool short of credit waits without holding a thread or an execution slot. Each operation carries a priority tier. A device's configuration doesn't hold a rate, in operations or in bytes per second: the work admitted to a device is bounded by its pools' capacities, which the device's answers set.

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

### DC-0011-045 {#DC-0011-045}

The device MUST start each pool's capacity at the capacity the device's configuration sets for that pool.

### DC-0011-046 {#DC-0011-046}

On a would-block answer to an operation, the device MUST lower the capacity of the pool the operation drew from to half its value, rounded up, or to the largest cost among the operations waiting for that pool's credit where that cost is higher.

### DC-0011-047 {#DC-0011-047}

The device MUST NOT lower a pool's capacity for a would-block answer to an operation that the pool granted before its last decrease.

### DC-0011-048 {#DC-0011-048}

When an operation whose cost is higher than its pool's capacity begins to wait for the pool's credit, the device MUST raise the pool's capacity to that operation's cost.

### DC-0011-055 {#DC-0011-055}

When a clean completion from a pool brings the summed cost of the clean completions in the pool's current run to at least the capacity the pool had when that run began, the device MUST raise the pool's capacity by the cost of that completion's operation, except where DC-0011-054 lowers the pool's capacity at that completion.

### DC-0011-056 {#DC-0011-056}

After the device starts a pool's capacity at its configured capacity, the device MUST change the capacity only as DC-0011-046, DC-0011-048, DC-0011-054 and DC-0011-055 require.

### DC-0011-051 {#DC-0011-051}

The device MUST change a pool's free credit by the same amount as each change of the pool's capacity.

### DC-0011-052 {#DC-0011-052}

A decrease of a pool's capacity MUST NOT take credit from an operation that holds it.

### DC-0011-054 {#DC-0011-054}

When a run of a pool's clean completions ends, its clean completions are all of operations that the pool granted after its last decrease, and its completion time is at least twice the shortest completion time above zero among the pool's earlier runs since that decrease whose clean completions are all of such operations, the device MUST lower the pool's capacity, at the completion that ends the run, to half its value, rounded up, or to the largest cost among the operations waiting for that pool's credit where that cost is higher.
