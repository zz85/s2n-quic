## The liveness refill {#refill}

The refill runs beside the distribution at a rate in bytes per second. Each tick it adds one quantum, the largest single acquire of any level, less the bytes released since the previous tick, so that under healthy load it adds nothing. Its ticks come at the interval in which the rate delivers one quantum, so it adds at most the rate on average. It never fills the pool past its capacity, counting the credit a pass holds back and the credit already added and not yet distributed. Credit it adds goes to waiting requests by the same rules as released credit. An endpoint turns the refill on by default at its send rate, and a holder can turn it off.

### DC-0008-030 {#DC-0008-030}

Where its holder sets a rate in bytes per second, a pool MUST run a liveness refill at that rate.

### DC-0008-031 {#DC-0008-031}

An endpoint SHOULD turn on the liveness refill of each pool it makes, at the endpoint's send rate.

### DC-0008-032 {#DC-0008-032}

Where a holder turns the refill off, the endpoint MUST keep it off.

### DC-0008-033 {#DC-0008-033}

The refill's quantum MUST be the largest single acquire of any priority level.

### DC-0008-034 {#DC-0008-034}

The refill MUST tick once in each interval in which its rate delivers one quantum.

### DC-0008-035 {#DC-0008-035}

At each tick, the refill MUST add the quantum less the bytes released since the previous tick, and at most the capacity less the pool's free credit, counting the credit held back or added and not yet distributed.

### DC-0008-036 {#DC-0008-036}

A wake of the distribution by a release or by its budget MUST NOT delay the refill's next tick.

### DC-0008-037 {#DC-0008-037}

Credit the refill adds MUST reach waiting requests by the same rules as credit a release returns.
