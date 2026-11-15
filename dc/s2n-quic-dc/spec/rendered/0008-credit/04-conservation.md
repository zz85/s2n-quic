## Conserving credit {#conservation}

The credit of a pool is in five places: free, which a new request can take at once; owed to waiting requests; returned by a release and not yet distributed; held back by a pass for its next pass; and held by holders. Free credit reads below zero while requests wait, by the amount they wait for. A pass that can't hand all returned credit to waiters without letting a new request take it holds the rest for its next pass. The liveness refill can add credit beyond the capacity for a time, so the pool clamps its free credit to the capacity.

### DC-0008-026 {#DC-0008-026}

Where the liveness refill hasn't added credit, a pool's free, owed, returned and held credit and the credit a pass held back for its next pass MUST sum to its capacity at every point where no request or release is in progress.

### DC-0008-027 {#DC-0008-027}

A pass MUST NOT grant more credit than it has left to distribute.

### DC-0008-028 {#DC-0008-028}

A pass MUST NOT raise the pool's free credit above zero while requests it owes credit to still wait.

### DC-0008-029 {#DC-0008-029}

A pool's free credit MUST NOT stay above its capacity after a pass.
