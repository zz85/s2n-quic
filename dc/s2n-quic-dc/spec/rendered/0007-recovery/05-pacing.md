## Pacing and sender load {#pacing}

Each sender's pacing rate, which it sets from its bandwidth estimate, sets its earliest departure time, the time before which its next data may not leave. Data frames wait for that time unless a probe carries them; acknowledgements and probes go at once, since they add little load and a delayed acknowledgement slows the peer. The earliest departure time is never more than one second in the future, so a degenerate pacing rate can't hold a sender for longer. The socket's own pacing, which RFC 0010 states, applies after this one. Each sender publishes a load score for the endpoint's choice among the senders of the same peer: the later of the current time and its earliest departure time, plus one smoothed round-trip time while the congestion window limits it, plus its queued bytes divided by its pacing rate. The published score reflects each change of the sender's queue, and shows an acknowledgement's effect on the window and the pacing rate only once every update of that acknowledgement is applied, so that the choice reads the sender's current state.

### DC-0007-033 {#DC-0007-033}

A sender MUST hold its data frames, other than those a probe carries, until its earliest departure time.

### DC-0007-034 {#DC-0007-034}

Acknowledgements and probes MUST NOT wait for pacing.

### DC-0007-035 {#DC-0007-035}

A sender's earliest departure time MUST NOT be more than one second in the future.

### DC-0007-036 {#DC-0007-036}

The system MUST publish for each sender a load score: the later of the current time and its earliest departure time, plus one smoothed round-trip time while the congestion window limits it, plus its queued bytes divided by its pacing rate.

### DC-0007-037 {#DC-0007-037}

A sender's published load score MUST reflect its queue as it stands after each change of the queue.

### DC-0007-041 {#DC-0007-041}

A sender's published load score MUST NOT show an acknowledgement's effect on its window or its pacing rate until every update that acknowledgement makes is applied.

### DC-0007-042 {#DC-0007-042}

Once every update an acknowledgement makes to a sender's window and pacing rate is applied, the sender's published load score MUST reflect them.
