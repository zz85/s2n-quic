## Receiving packets {#receive}

The receiver keeps receive state for each pair of peer credential and sender identifier, since each sender of a peer numbers its packets in its own space. A packet is authenticated with the path secret its credential names and checked against the packet numbers already accepted from its sender, and only then is the payload of its frames delivered to their streams, in the order the packet holds them. When the receiver can't use a packet's credential, it tells the sender why, through the notices RFC 0002 defines.

### DC-0005-007 {#DC-0005-007}

A receiver MUST keep separate packet number and acknowledgement state for each pair of peer credential and sender identifier.

### DC-0005-008 {#DC-0005-008}

A receiver MUST NOT deliver to a stream the payload of a frame whose packet fails authentication.

### DC-0005-009 {#DC-0005-009}

A receiver MUST NOT acknowledge a packet that fails authentication.

### DC-0005-010 {#DC-0005-010}

A receiver MUST NOT dispatch the frames of a packet whose packet number it has already accepted from the same sender, or whose packet number is too old for it to tell.

### DC-0005-011 {#DC-0005-011}

When a receiver doesn't hold the path secret that a packet's credential names, it MUST send the packet's sender an unknown path secret notice at the sender's control port.

### DC-0005-012 {#DC-0005-012}

When the first packet a receiver authenticates under a new key fails the replay check, the receiver MUST send the packet's sender a replay notice.

### DC-0005-013 {#DC-0005-013}

A receiver MUST NOT keep receive state for a new key until a packet under that key has passed both authentication and the replay check.

### DC-0005-014 {#DC-0005-014}

A receiver MUST dispatch the frames of an accepted packet in the order the packet carries them.

### DC-0005-015 {#DC-0005-015}

A receiver MUST stop dispatching a packet's frames at the first frame whose declared length runs past the end of the packet's payload.

### DC-0005-016 {#DC-0005-016}

A receiver MUST count each packet it accepts as activity from the packet's peer.
