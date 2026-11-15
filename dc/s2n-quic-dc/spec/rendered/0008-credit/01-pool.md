## The credit pool {#pool}

A credit pool holds a capacity of credit, counted in bytes. A holder acquires credit before it commits bytes, such as bytes it sends or buffers, and releases the credit once those bytes leave its care. Each request has one of eight priority levels: highest, high, medium-high, medium, medium-low, low, lowest and background. Each level has a largest single acquire, which bounds one request, and a smallest grant, which bounds how little a waiting request is given at once.

### DC-0008-001 {#DC-0008-001}

A credit pool MUST have a capacity in bytes, set when the pool is made.

### DC-0008-002 {#DC-0008-002}

A credit pool MUST order its requests by eight priority levels, from highest to background.

### DC-0008-003 {#DC-0008-003}

Each priority level MUST have a largest single acquire of at least one byte and at most the capacity.

### DC-0008-004 {#DC-0008-004}

Each priority level MUST have a smallest grant of at least one byte and at most that level's largest single acquire.

### DC-0008-005 {#DC-0008-005}

A pool MUST bring each largest single acquire and each smallest grant that a holder sets outside its bounds to the nearest bound.

### DC-0008-006 {#DC-0008-006}

Where a holder doesn't set the capacity, a pool SHOULD have a capacity of 256 MiB.

### DC-0008-007 {#DC-0008-007}

Where a holder doesn't set it, the largest single acquire SHOULD be the capacity divided by 256 at the four highest levels and the capacity divided by 64 at the four lowest.

### DC-0008-008 {#DC-0008-008}

Where a holder doesn't set it, the smallest grant of each level SHOULD be 64 KiB, or the level's largest single acquire where that is smaller.
