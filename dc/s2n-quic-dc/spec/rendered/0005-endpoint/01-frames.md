## Frames and their outcomes {#frames}

A frame is the unit of work a stream submits to the endpoint: a header that says what the frame is and which stream it is for, and an optional payload. The endpoint decides how frames are packed into packets. Every frame that its writer still waits on ends in one outcome that the writer can observe: acknowledged by the peer, or failed with a reason. The reasons are that the peer was declared dead, a transmission error, such as the frame reaching its bound on transmissions, and that the peer doesn't know the path secret. A frame its writer cancelled ends without a report, since nobody waits for it. Frames fall into priority classes, from acknowledgements through control frames to stream data.

### DC-0005-001 {#DC-0005-001}

A sender MAY carry several frames, of one or more streams to the same peer, in one packet.

### DC-0005-002 {#DC-0005-002}

For each frame its writer hasn't cancelled, the endpoint MUST report exactly one outcome to the writer: acknowledged, or failed with its reason.

### DC-0005-003 {#DC-0005-003}

The endpoint MUST NOT transmit a frame whose writer has cancelled it or gone away.

### DC-0005-004 {#DC-0005-004}

The endpoint MUST bound the number of transmissions of each frame.

### DC-0005-051 {#DC-0005-051}

When a frame reaches its bound on transmissions without an acknowledgement, the endpoint MUST report the frame to its writer as failed with the transmission error reason.

### DC-0005-005 {#DC-0005-005}

A sender MUST send a peer's pending frames of a higher priority class before its pending frames of a lower priority class.

### DC-0005-006 {#DC-0005-006}

A sender MUST NOT hold back an acknowledgement because the congestion window is full.
