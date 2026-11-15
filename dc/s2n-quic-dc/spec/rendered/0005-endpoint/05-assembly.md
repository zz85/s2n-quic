## Assembling and sending packets {#assembly}

When a peer's frames are due, the sender packs as many of them as fit into packets of at most the path's packet size and within the congestion window, protects each packet, sends it, and records which frames it carried until it is acknowledged or declared lost. A packet's routing fields and frame metadata travel unencrypted and are authenticated; the frame payloads are encrypted. A sender may zero-pad a datagram after the last packet's tag; the padding isn't part of the packet, and the receiver ignores the bytes after the tag. When the probe timeout of RFC 0007 fires, the sender sends probes so that the peer answers. RFC 0007 sets the congestion window, the pacing and the probe timeout.

### DC-0005-032 {#DC-0005-032}

A sender MUST NOT send a packet larger than the maximum packet size of the path.

### DC-0005-033 {#DC-0005-033}

A sender MUST number each packet it sends under one sender identifier higher than every packet it sent before under that identifier.

### DC-0005-034 {#DC-0005-034}

A sender MUST encrypt the frame payloads of each packet.

### DC-0005-052 {#DC-0005-052}

A sender MUST authenticate every byte of each packet that carries frames, its unencrypted header and frame metadata among them.

### DC-0005-035 {#DC-0005-035}

A sender MUST record which frames each packet carried until the packet is acknowledged or declared lost.

### DC-0005-036 {#DC-0005-036}

When its probe timeout fires, a sender MUST send two ack-eliciting packets to the peer.

### DC-0005-037 {#DC-0005-037}

When a probe is due and no frames are pending for the peer, the sender MUST make the probe from frames of the oldest packet still awaiting acknowledgement, sent again under a new packet number.

### DC-0005-038 {#DC-0005-038}

A sender SHOULD send waiting acknowledgements and probes without holding them behind paced data frames.

### DC-0005-039 {#DC-0005-039}

A sender MAY spread one peer's frames across several of its senders, each with its own sender identifier and packet number space.
