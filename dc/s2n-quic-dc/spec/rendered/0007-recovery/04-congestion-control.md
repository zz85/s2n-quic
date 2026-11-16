## Congestion control {#congestion-control}

Each sender has one congestion controller, created with the path's maximum datagram size. The controller is model-based: it estimates the path's bottleneck bandwidth and its minimum round-trip time from the acknowledgements it receives, and sets the congestion window, the pacing rate and the send quantum from those estimates, with loss and congestion marks as further signals. A sender that has no more application data queued doesn't lower its rate or its window for the room it leaves unused, so a quiet sender isn't read as a slow path. A run of consecutive lost packet numbers counts as one loss event, and each increase of the peer's count of packets marked congestion experienced counts as a sign of congestion. A sender that probes above its bandwidth estimate ends the probe once more than half of the data an acknowledgement shows delivered was marked, and caps its window near the total size of its outstanding packets when it sent that data. The sender takes an acknowledgement's congestion marks and losses into account before it raises its rate or its window for the data that acknowledgement acknowledges. The sender doesn't treat a long silence as persistent congestion: the probe timer and the model cover it. The sender sends new data frames only while it has room for new data, as a fast retransmission, or when the frames make up a probe, and each send carries at most the send quantum. The room for new data is the window less the total size of the outstanding packets, leaving out each packet whose frames a probe has carried again, and it is never less than zero.

### DC-0007-023 {#DC-0007-023}

The system MUST give each sender one window and one sending rate, set from the acknowledgements, losses and congestion marks of all of that sender's packets.

### DC-0007-024 {#DC-0007-024}

Except when it sends a fast retransmission or a probe, a sender MUST send a packet that becomes outstanding only where the total size of its outstanding packets whose frames no probe has carried again, that packet included, stays within a window that it sets from the path's bottleneck bandwidth and minimum round-trip time, as it measures them from the acknowledgements it receives.

### DC-0007-040 {#DC-0007-040}

The rate at which a sender sends data MUST follow the path's bottleneck bandwidth, as the sender measures it from the acknowledgements it receives.

### DC-0007-025 {#DC-0007-025}

A sender MUST NOT lower its sending rate or its window because it sent less than its window allows while it had no application data queued.

### DC-0007-026 {#DC-0007-026}

A sender MUST respond to a run of consecutive lost packet numbers as it responds to one lost packet.

### DC-0007-027 {#DC-0007-027}

A sender MUST lower a limit that caps its window, and that never caps it below the sender's minimum window, within the round trip after one in which the peer's count of packets marked congestion experienced increased, unless at some point in that round trip the sender was starting up, draining the queue its start-up built, probing for more bandwidth, or probing its round-trip time.

### DC-0007-043 {#DC-0007-043}

A sender that is starting up MUST end its start-up, in which it raises its sending rate each round trip, after two consecutive round trips in each of which the packets marked congestion experienced, each counted at the path's maximum datagram size, made up more than half of the data delivered.

### DC-0007-044 {#DC-0007-044}

A sender that is sending above its estimate of the path's bottleneck bandwidth to probe for more bandwidth MUST end that probe and lower its sending rate below its estimate on an acknowledgement for which the packets marked congestion experienced, each counted at the path's maximum datagram size, made up more than half of the data delivered between the send of the newest packet that acknowledgement acknowledges and its arrival.

### DC-0007-045 {#DC-0007-045}

On an acknowledgement that ends a probe as DC-0007-044 describes, the sender MUST set a limit that caps its window, and that never caps it below the sender's minimum window, to the larger of the total size of its outstanding packets whose frames no probe had carried again when it sent the newest packet that acknowledgement acknowledges and seven tenths of the smaller of its window and its estimate of the path's bandwidth-delay product, unless it sent that packet within an application-limited period: one that opens when the sender runs out of queued application data while no such period is open, and that closes once the data the sender has seen acknowledged, declared lost or discarded since the period opened exceeds the total size of its outstanding packets whose frames no probe had carried again when the period opened.

### DC-0007-028 {#DC-0007-028}

A sender MUST apply an acknowledgement's losses and congestion marks to its sending rate and its window before it raises either of them for the data that acknowledgement acknowledges.

### DC-0007-029 {#DC-0007-029}

A sender MUST NOT send new data frames while it has no room for new data, unless it sends them as a fast retransmission or as a probe.

### DC-0007-030 {#DC-0007-030}

Each send of a sender MUST put at most its send quantum on the wire, counted in datagrams of the path's maximum size and rounded up.

### DC-0007-031 {#DC-0007-031}

The room a sender has for new data MUST be the larger of zero and its window less the total size of its outstanding packets whose frames no probe has carried again.

### DC-0007-032 {#DC-0007-032}

A packet whose frames a probe carries again MUST NOT take room in the window once the probe is sent.
