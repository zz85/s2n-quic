## Signals of a pool {#signals}

A pool reports gauges and counters under a prefix its holder sets, such as one prefix for the pool of sending and another for the pool of receiving. Its free credit reads below zero by the backlog of waiting requests, which is load, not an error. Its held credit is the leak signal: a large value under load is healthy, but held credit that stays above zero while traffic is idle and no request waits is credit that left and didn't come back. A gauge is computed when it is read, so a sample can be off by changes made while it is read; the steady value across samples is the signal.

### DC-0008-046 {#DC-0008-046}

A pool MUST report its signals under a prefix its holder sets.

### DC-0008-047 {#DC-0008-047}

A pool MUST report its free credit, which reads below zero by the credit waiting requests are owed.

### DC-0008-048 {#DC-0008-048}

A pool MUST report the credit that waiting requests are owed and haven't been granted.

### DC-0008-049 {#DC-0008-049}

A pool MUST report the credit returned by releases and not yet distributed.

### DC-0008-050 {#DC-0008-050}

A pool MUST report the credit its holders hold, as its capacity less its free, owed and returned credit.

### DC-0008-051 {#DC-0008-051}

A pool MUST count the requests of more than 0 bytes it granted at once.

### DC-0008-052 {#DC-0008-052}

A pool MUST count the bytes requested: each request's amount, lowered to its level's largest single acquire, when it is granted at once or begins to wait.

### DC-0008-053 {#DC-0008-053}

A pool MUST count the distribution's passes.

### DC-0008-054 {#DC-0008-054}

A pool MUST report the credit a pass held back for the next pass.

### DC-0008-055 {#DC-0008-055}

A pool MUST count the bytes the refill added.

### DC-0008-070 {#DC-0008-070}

A pool MUST count, at each priority level, the requests that waited.

### DC-0008-071 {#DC-0008-071}

A pool MUST count the bytes released.

### DC-0008-072 {#DC-0008-072}

A pool MUST count each release of more than 0 bytes.

### DC-0008-073 {#DC-0008-073}

A pool MUST count the distribution's grants.

### DC-0008-074 {#DC-0008-074}

A pool MUST count the cancelled waits the distribution reclaimed.

### DC-0008-075 {#DC-0008-075}

A pool MUST count the races of a cancel and a grant.

### DC-0008-076 {#DC-0008-076}

A pool MUST count the distribution's turns that ended on their budget.

### DC-0008-077 {#DC-0008-077}

A pool MUST count each clamp of its free credit to its capacity.

### DC-0008-078 {#DC-0008-078}

A pool MUST count the refill's ticks that added credit.

### DC-0008-079 {#DC-0008-079}

A pool MUST count the refill's ticks that added no credit.

### DC-0008-080 {#DC-0008-080}

A pool MUST report how many ticks in a row the refill added credit.

### DC-0008-082 {#DC-0008-082}

Where a refill is set, a pool MUST report, at each refill tick, how far its free credit is below its capacity.
