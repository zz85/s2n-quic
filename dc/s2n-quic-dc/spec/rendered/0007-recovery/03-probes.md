## Probe timeout {#probes}

When packets are outstanding and acknowledgements stop arriving, such as when the last packets of a burst are lost, an acknowledgement can't trigger loss detection. The probe timer covers that case. Its base period is QUIC's probe timeout, computed from the round-trip time estimate, and it is measured from the most recent packet send. When it elapses, the sender sends a probe of two ack-eliciting packets, so that their acknowledgement lets the packet threshold find the packets lost before them. When data is queued, the probe carries that data. When no data is queued, the probe carries the frames of the oldest outstanding packet that a probe hasn't carried yet and that holds a frame that isn't cancelled, under a new packet number, and the frames that don't fit in the probe go back to the sender's queue. The original packet's record stays as a link to the probe, so a late acknowledgement of the original still completes its frames, while a loss of the original leaves them to the probe. Each probe doubles the wait for the next one, up to 32 base periods, and each acknowledgement resets the wait to one base period. The timer is armed only while packets are outstanding, and once outstanding bytes reach zero, the records left as links are discarded.

### DC-0007-013 {#DC-0007-013}

While packets are outstanding, the system MUST send a probe once the probe timeout elapses without an acknowledgement.

### DC-0007-014 {#DC-0007-014}

Each probe MUST consist of two ack-eliciting packets.

### DC-0007-015 {#DC-0007-015}

After each probe, the system MUST double the time until the next probe, up to 32 times the base probe timeout.

### DC-0007-016 {#DC-0007-016}

On each acknowledgement, the system MUST reset the probe backoff to the base probe timeout.

### DC-0007-017 {#DC-0007-017}

The system MUST NOT arm the probe timer while no packet is outstanding.

### DC-0007-018 {#DC-0007-018}

When a probe is due and no data is queued, the probe MUST carry the frames of the oldest outstanding packet that a probe hasn't carried yet and that holds a frame that isn't cancelled, under a new packet number.

### DC-0007-019 {#DC-0007-019}

An acknowledgement of a packet whose frames a probe carried MUST complete those frames as an acknowledgement of the probe would.

### DC-0007-020 {#DC-0007-020}

Once a probe carries a packet's frames, the system MUST treat those frames as frames of the probe's packet.

### DC-0007-021 {#DC-0007-021}

The system MUST discard each record of a packet whose frames a probe carried no later than when the outstanding bytes reach zero.

### DC-0007-022 {#DC-0007-022}

The system MUST measure the probe timeout from the most recent packet send.

### DC-0007-038 {#DC-0007-038}

When a probe is due and data is queued, the probe MUST carry the queued data.

### DC-0007-039 {#DC-0007-039}

The system MUST return to the sender's queue each frame of the probed packet that doesn't fit in the probe.
