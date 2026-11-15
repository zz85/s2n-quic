## Congestion control {#congestion-control}

Each sender has one congestion controller, created with the path's maximum datagram size. The controller is model-based: it estimates the path's bottleneck bandwidth and its minimum round-trip time from the acknowledgements it receives, and sets the congestion window, the pacing rate and the send quantum from those estimates, with loss and congestion marks as further signals. The sender tells it the size of each packet it sends and whether more application data waits behind it, so that a sender with nothing to send isn't read as a slow path. For each acknowledgement, the sender updates the round-trip time estimate, reports each increase of the peer's count of packets marked congestion experienced, runs loss detection and reports each lost packet with whether it begins a new run of lost packet numbers, and only then reports the bytes the acknowledgement newly acknowledges. The sender doesn't report persistent congestion: the probe timer and the model cover a long silence. The sender sends new data frames only while the window has room, while the controller asks for fast retransmission, or when the frames make up a probe, and each send carries at most the send quantum. Outstanding bytes count each outstanding packet once: when a probe carries a packet's frames, the original's bytes leave the count.

### DC-0007-023 {#DC-0007-023}

The system MUST keep one congestion controller for each sender.

### DC-0007-024 {#DC-0007-024}

The congestion controller MUST estimate the path's bottleneck bandwidth and its minimum round-trip time from acknowledgements, and set the congestion window and the pacing rate from those estimates.

### DC-0007-025 {#DC-0007-025}

The system MUST tell the controller the size of each packet it sends, and whether the sender has more application data queued.

### DC-0007-026 {#DC-0007-026}

The system MUST report each lost packet's bytes to the controller, with whether it begins a new run of lost packet numbers.

### DC-0007-027 {#DC-0007-027}

The system MUST report to the controller each increase of the peer's count of packets marked congestion experienced.

### DC-0007-028 {#DC-0007-028}

The system MUST give the controller an acknowledgement's newly acknowledged bytes only after it has run loss detection and read the congestion marks of that acknowledgement.

### DC-0007-029 {#DC-0007-029}

A sender MUST NOT send new data frames while its controller reports it limited by the congestion window, unless the controller asks for fast retransmission or the frames make up a probe.

### DC-0007-030 {#DC-0007-030}

The system MUST bound each send of a sender to its controller's send quantum, in datagrams of the path's maximum size, rounded up.

### DC-0007-031 {#DC-0007-031}

The outstanding bytes that the controller holds MUST equal the total size of the outstanding packets whose bytes still count.

### DC-0007-032 {#DC-0007-032}

Once a probe carries a packet's frames, the system MUST remove that packet's bytes from the outstanding bytes.
