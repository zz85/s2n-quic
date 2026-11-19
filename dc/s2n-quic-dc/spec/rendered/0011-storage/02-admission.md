## Admission by credit {#admission}

Each device holds one or two credit pools. A pool's capacity starts at the capacity the device's configuration sets for it, the device's queue depth, and changes at run time from the device's answers. A would-block answer, such as EAGAIN, says that the device can't take an operation now; it halves the capacity of the pool the operation drew from, and the would-block answers to operations that pool granted before that decrease don't lower it again. A clean completion is the completion of an operation that the device took without a would-block answer, that the pool didn't grant alone for its cost, and that returns its credit while a submitter waits for credit from the same pool. A pool's first run of clean completions begins when its device is created, and a new run begins at each would-block answer to an operation that drew from the pool, at each raise of the pool's capacity and at the end of the run before it. A run ends at the clean completion that brings the summed cost of the run's clean completions to at least the capacity the pool had when the run began, and that completion raises the capacity by its operation's cost. The capacity the pool had when a run began is the capacity as the event that began the run leaves it, after any decrease or raise that event makes, so a run that begins at a raise from 8 to 12 ends at the clean completion that brings its summed cost to at least 12, and a run that begins at a would-block answer that halves the capacity from 8 to 4 ends at the one that brings it to at least 4. A run's completion time is the longest time from the pool's grant of an operation's credit to that operation's completion among the operations of the run's clean completions, and a pool's last decrease is the latest lowering of its capacity or of its shown capacity, or the device's creation where the device hasn't lowered either yet. A run whose clean completions are all of operations that the pool granted after its last decrease, and whose completion time is at least twice the shortest completion time above zero among the pool's earlier runs of that kind since that decrease, shows the device's completion latency rising: the completion that ends that run halves the capacity, as a would-block answer does, instead of raising it. An operation that waits for a pool's credit is at the front of the pool when no operation of its priority tier or of a higher tier that began to wait before it still waits. A pool's shown capacity is the most work the device has shown that it takes from the pool at once: it starts at the configured capacity; at each raise at the end of a run, it rises to the capacity that raise leaves the pool at or to twice its value, whichever is lower; at the completion of each operation that the pool granted alone for its cost and that the device took without a would-block answer, it rises to that operation's cost or to twice its value, whichever is lower; and each would-block answer to an operation that the pool granted after its last decrease, and each run whose completion latency rises, halves it, rounded up, whether or not the capacity falls. So the shown capacity at most doubles at each clean answer and falls at each sign of congestion, and it follows the device's current answers rather than one past answer. A decrease never takes the capacity below the cost of an operation that waits for the pool's credit and whose cost isn't higher than the shown capacity as that decrease leaves it. An operation whose cost is higher than the capacity, and not higher than the shown capacity, raises the capacity to its cost as it begins to wait. An operation whose cost is higher than the shown capacity doesn't change the capacity: once it is at the front, no other operation holds the pool's credit and no operation of a higher tier waits, the pool grants it its whole cost alone, which takes the pool's free credit below zero, and the pool grants nothing else until that operation returns its credit. While it waits at the front, the pool doesn't grant an operation of its tier or a lower tier, and grants an operation of a higher tier only within the shown capacity; where the device answers it would-block and it waits again, it waits behind the operations of its tier that waited when that answer came. The device doesn't reject an operation for its cost. A buffered operation's buffer is allocated only once the pool grants the operation's credit. A device's configuration doesn't set an upper bound on a pool's capacity: only the device's answers stop the capacity from rising. Each change of the capacity changes the pool's free credit by the same amount, so a lower capacity takes its difference back from the credit that completing operations return, and never from an operation that holds credit. Where this RFC says a pool's capacity, it means its current capacity, and the configured capacity is the value it starts at. An operation's cost, in credits, is taken from a pool before the operation enters the device's execution pipeline and is returned when the operation completes. A submitter that finds the pool short of credit waits without holding a thread or an execution slot. Each operation carries a priority tier. A device's configuration doesn't hold a rate, in operations or in bytes per second: the work admitted to a device is bounded by its pools' capacities, which the device's answers set.

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

On a would-block answer to an operation, the device MUST lower the capacity of the pool the operation drew from to half its value, rounded up, or to the largest cost among the operations waiting for that pool's credit where that cost is higher, leaving out each operation whose cost is higher than the pool's shown capacity as DC-0011-062 lowers it at that answer.

### DC-0011-047 {#DC-0011-047}

The device MUST NOT lower a pool's capacity for a would-block answer to an operation that the pool granted before its last decrease.

### DC-0011-048 {#DC-0011-048}

When an operation whose cost is higher than its pool's capacity, and not higher than its pool's shown capacity, begins to wait for the pool's credit, the device MUST raise the pool's capacity to that operation's cost.

### DC-0011-057 {#DC-0011-057}

The device MUST grant an operation whose cost is higher than its pool's shown capacity only while no other operation holds credit from that pool.

### DC-0011-058 {#DC-0011-058}

When an operation that DC-0011-057 granted alone completes and the device took it without a would-block answer, the device MUST raise its pool's shown capacity to that operation's cost or to twice the shown capacity, whichever is lower.

### DC-0011-059 {#DC-0011-059}

The device MUST NOT reject an operation because its cost is higher than its pool's capacity or its pool's shown capacity.

### DC-0011-060 {#DC-0011-060}

The device MUST NOT allocate the buffer of a buffered operation before the operation's pool grants its credit.

### DC-0011-061 {#DC-0011-061}

While an operation whose cost is higher than its pool's shown capacity waits at the front of that pool, the device MUST NOT grant an operation of its priority tier or of a lower tier from that pool.

### DC-0011-062 {#DC-0011-062}

At each would-block answer to an operation that its pool granted after the pool's last decrease, and at each completion at which DC-0011-054 requires a decrease, the device MUST lower the pool's shown capacity to half its value, rounded up, whether or not the pool's capacity falls.

### DC-0011-063 {#DC-0011-063}

When a clean completion raises a pool's capacity as DC-0011-055 requires, the device MUST raise the pool's shown capacity to the capacity that raise leaves the pool at or to twice the shown capacity, whichever is lower, where that is higher than the shown capacity.

### DC-0011-064 {#DC-0011-064}

When an operation whose cost is higher than its pool's shown capacity is at the front of the pool, no other operation holds credit from that pool and no operation of a higher priority tier waits for that pool's credit, the device MUST grant that operation its whole cost, though its cost is higher than the pool's free credit, without changing the pool's capacity.

### DC-0011-065 {#DC-0011-065}

While an operation whose cost is higher than its pool's shown capacity waits at the front of that pool, the device MUST grant an operation of a higher priority tier whose cost isn't higher than the shown capacity only while the credit that operations hold from the pool, with that operation's cost, is at most the shown capacity.

### DC-0011-066 {#DC-0011-066}

While an operation that DC-0011-064 granted holds credit from its pool, the device MUST NOT grant another operation from that pool.

### DC-0011-067 {#DC-0011-067}

The device MUST start each pool's shown capacity at the capacity the device's configuration sets for that pool.

### DC-0011-068 {#DC-0011-068}

After the device starts a pool's shown capacity at its configured capacity, the device MUST change the shown capacity only as DC-0011-058, DC-0011-062 and DC-0011-063 require.

### DC-0011-069 {#DC-0011-069}

When the device answers would-block to an operation that DC-0011-057 granted alone and that operation waits for its pool's credit again, the device MUST grant it after each operation of its priority tier that waited for that pool's credit when that answer came.

### DC-0011-055 {#DC-0011-055}

When a clean completion from a pool brings the summed cost of the clean completions in the pool's current run to at least the capacity the pool had when that run began, the device MUST raise the pool's capacity by the cost of that completion's operation, except where DC-0011-054 lowers the pool's capacity at that completion.

### DC-0011-056 {#DC-0011-056}

After the device starts a pool's capacity at its configured capacity, the device MUST change the capacity only as DC-0011-046, DC-0011-048, DC-0011-054 and DC-0011-055 require.

### DC-0011-051 {#DC-0011-051}

The device MUST change a pool's free credit by the same amount as each change of the pool's capacity.

### DC-0011-052 {#DC-0011-052}

A decrease of a pool's capacity MUST NOT take credit from an operation that holds it.

### DC-0011-054 {#DC-0011-054}

When a run of a pool's clean completions ends, its clean completions are all of operations that the pool granted after its last decrease, and its completion time is at least twice the shortest completion time above zero among the pool's earlier runs since that decrease whose clean completions are all of such operations, the device MUST lower the pool's capacity, at the completion that ends the run, to half its value, rounded up, or to the largest cost among the operations waiting for that pool's credit where that cost is higher, leaving out each operation whose cost is higher than the pool's shown capacity as DC-0011-062 lowers it at that completion.
