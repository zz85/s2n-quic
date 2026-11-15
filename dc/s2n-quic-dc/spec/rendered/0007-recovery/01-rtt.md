## Round-trip time estimation {#rtt}

Each sender keeps its own estimate of the round-trip time to its peer: the latest sample, a smoothed value, the variation of the samples and the minimum sample, maintained as QUIC maintains them. A sample comes from each acknowledgement that newly acknowledges a packet carrying data: the time from the send of the newest such packet to the acknowledgement's arrival, less the acknowledgement delay the peer reports, and never less than one microsecond. A sender that mostly reads sends few data packets, and keeps its estimate fresh from the acknowledgement-only probes that RFC 0005 describes. Until a sample arrives, the estimate starts at 2 milliseconds, a value suited to a data-center network, so that the first probe after an early loss follows within a few milliseconds. The estimate is updated before loss detection runs, since the loss time threshold depends on it.

### DC-0007-001 {#DC-0007-001}

The system MUST keep a round-trip time estimate for each sender, holding its latest sample, a smoothed value, the variation of the samples and the minimum sample.

### DC-0007-002 {#DC-0007-002}

On an acknowledgement that newly acknowledges a data packet, the system MUST take a round-trip time sample from the newest packet it newly acknowledges: the time from that packet's send to the acknowledgement's arrival, less the acknowledgement delay the peer reports.

### DC-0007-003 {#DC-0007-003}

A round-trip time sample MUST NOT be less than one microsecond.

### DC-0007-004 {#DC-0007-004}

On an acknowledgement that doesn't newly acknowledge a data packet but covers an acknowledgement-only probe, the system MUST take the round-trip time sample from that probe.

### DC-0007-005 {#DC-0007-005}

Before its first sample, the system MUST use an initial round-trip time of 2 milliseconds.

### DC-0007-006 {#DC-0007-006}

The system MUST update the round-trip time estimate from an acknowledgement before it runs loss detection for that acknowledgement.
