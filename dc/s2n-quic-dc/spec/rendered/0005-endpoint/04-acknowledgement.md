## Acknowledging packets {#acknowledgement}

The receiver acknowledges every packet that carries an ack-eliciting frame. Every frame other than an acknowledgement is ack-eliciting; an acknowledgement can be marked ack-eliciting to ask the peer for a response. An acknowledgement carries the ranges of packet numbers received from one sender, the explicit congestion notification counts of those packets, and the delay between the receipt of the largest packet it covers and its own assembly. The sender applies it to the packets it recorded: the frames of each acknowledged packet are complete, and the frames of each packet declared lost are recovered one by one. RFC 0007 says when a packet is declared lost.

### DC-0005-022 {#DC-0005-022}

A receiver MUST acknowledge each packet it accepts that carries an ack-eliciting frame.

### DC-0005-023 {#DC-0005-023}

A receiver MUST NOT send an acknowledgement in response to a packet that doesn't carry an ack-eliciting frame.

### DC-0005-024 {#DC-0005-024}

A receiver MUST have at most one acknowledgement for each sender waiting to be sent at any time.

### DC-0005-025 {#DC-0005-025}

When an ack-eliciting packet arrives while an acknowledgement for its sender is waiting to be sent, the receiver MUST send a further acknowledgement that covers it after the waiting one is sent.

### DC-0005-026 {#DC-0005-026}

An acknowledgement MUST carry the sender identifier of the packets it acknowledges.

### DC-0005-027 {#DC-0005-027}

An acknowledgement MUST carry the counts of explicit congestion notification marks on the packets received from its sender.

### DC-0005-028 {#DC-0005-028}

On an acknowledgement, a sender MUST report each frame of each newly acknowledged packet to its writer as acknowledged.

### DC-0005-029 {#DC-0005-029}

When a packet is declared lost, the sender MUST queue each of its frames for retransmission, except a frame that has been cancelled or has reached its bound on transmissions.

### DC-0005-030 {#DC-0005-030}

When a sender has no packet that carries data awaiting acknowledgement, it MAY mark one acknowledgement it sends as ack-eliciting to measure the round-trip time.

### DC-0005-031 {#DC-0005-031}

After an acknowledgement that a sender marked ack-eliciting to measure the round-trip time yields a sample, the sender MUST NOT mark another acknowledgement as ack-eliciting until it has sent a packet that carries data.
