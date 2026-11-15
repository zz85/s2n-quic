## Loss detection {#loss-detection}

A packet stays outstanding until an acknowledgement covers it or loss detection declares it lost. Loss detection runs on each acknowledgement that acknowledges a packet, and uses two thresholds. By the packet threshold, a packet is lost when its packet number is at least two less than the largest packet number the acknowledgement covers. By the time threshold, a packet is lost when it was sent at least the loss time threshold before the newest packet the acknowledgement acknowledges; the loss time threshold is nine-eighths of the larger of the smoothed and the latest round-trip time, and at least 1 millisecond. Loss is declared from the oldest outstanding packet onward, and the scan stops at the first packet that meets neither threshold. The frames of each lost packet are recovered one by one, as RFC 0005 says. A packet's record holds only its ack-eliciting frames: acknowledgement frames are never retransmitted, since a later acknowledgement replaces them.

### DC-0007-007 {#DC-0007-007}

The system MUST NOT declare a packet lost before it receives an acknowledgement of a packet with a larger packet number.

### DC-0007-008 {#DC-0007-008}

The system MUST declare lost each outstanding packet whose packet number is at least two less than the largest packet number an acknowledgement covers.

### DC-0007-009 {#DC-0007-009}

The system MUST declare lost each outstanding packet sent at least the loss time threshold before the newest packet that an acknowledgement acknowledges.

### DC-0007-010 {#DC-0007-010}

The loss time threshold MUST be nine-eighths of the larger of the smoothed and the latest round-trip time, and at least 1 millisecond.

### DC-0007-011 {#DC-0007-011}

The system MUST NOT declare a packet lost while an outstanding packet with a smaller packet number isn't declared lost.

### DC-0007-012 {#DC-0007-012}

A sent packet's record MUST hold only its frames other than acknowledgement frames.
