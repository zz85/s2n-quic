## Congestion control {#congestion-control}

Each sender has one congestion controller, created with the path's maximum datagram size. The controller is model-based: it estimates the path's bottleneck bandwidth and its minimum round-trip time from the acknowledgements it receives, and sets the congestion window, the pacing rate and the send quantum from those estimates, with loss and congestion marks as further signals. A sender that has no more application data queued doesn't lower its rate or its window for the room it leaves unused, so a quiet sender isn't read as a slow path. A run of consecutive lost packet numbers counts as one loss event, and each increase of the peer's count of packets marked congestion experienced counts as a sign of congestion. The sender takes an acknowledgement's congestion marks and losses into account before it raises its rate or its window for the data that acknowledgement acknowledges. The sender doesn't treat a long silence as persistent congestion: the probe timer and the model cover it. The sender sends new data frames only while the window has room, while the controller asks for fast retransmission, or when the frames make up a probe, and each send carries at most the send quantum. The bytes counted against the window count each outstanding packet once: when a probe carries a packet's frames, the original's bytes leave the count.

### DC-0007-023 {#DC-0007-023}

The system MUST keep one congestion controller for each sender.

### DC-0007-024 {#DC-0007-024}

Except for data it sends as a fast retransmission or as a probe, the data a sender has sent and not yet seen acknowledged MUST stay within a window that the sender sets from the path's bottleneck bandwidth and minimum round-trip time, as it measures them from the acknowledgements it receives.

### DC-0007-040 {#DC-0007-040}

The rate at which a sender sends data MUST follow the path's bottleneck bandwidth, as the sender measures it from the acknowledgements it receives.

### DC-0007-025 {#DC-0007-025}

A sender MUST NOT lower its sending rate or its window because it sent less than its window allows while it had no application data queued.

### DC-0007-026 {#DC-0007-026}

A sender MUST count a run of consecutive lost packet numbers as one loss event when it judges whether loss limits its sending.

### DC-0007-027 {#DC-0007-027}

A sender MUST lower its sending rate or its window in response to an increase of the peer's count of packets marked congestion experienced.

### DC-0007-028 {#DC-0007-028}

A sender MUST take an acknowledgement's losses and congestion marks into account before it raises its sending rate or its window for the data that acknowledgement acknowledges.

### DC-0007-029 {#DC-0007-029}

A sender MUST NOT send new data frames while its controller reports it limited by the congestion window, unless the controller asks for fast retransmission or the frames make up a probe.

### DC-0007-030 {#DC-0007-030}

The system MUST bound each send of a sender to its controller's send quantum, in datagrams of the path's maximum size, rounded up.

### DC-0007-031 {#DC-0007-031}

The bytes a sender counts against its window MUST equal the total size of its outstanding packets, each counted once.

### DC-0007-032 {#DC-0007-032}

Once a probe carries a packet's frames, the system MUST remove that packet's bytes from the outstanding bytes.
