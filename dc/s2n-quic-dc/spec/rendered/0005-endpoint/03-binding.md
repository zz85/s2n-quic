## Binding server streams {#binding}

A client opens a stream by sending frames marked as opening frames: a data frame, a message frame or a reset. Each opening frame carries the acceptor that is to receive the new stream and the credit tier the stream is to use. The server binds the stream on the first opening frame that arrives; once the server confirms the binding, the client sends later frames without the opening fields.

### DC-0005-017 {#DC-0005-017}

A server MUST bind a new stream on the first opening frame for it that arrives, whether that frame is a data frame, a message frame or a reset.

### DC-0005-018 {#DC-0005-018}

A client MUST carry the same acceptor and the same credit tier on every opening frame of a stream.

### DC-0005-019 {#DC-0005-019}

A server MUST NOT bind a stream again when an opening frame arrives for a stream it has already bound.

### DC-0005-020 {#DC-0005-020}

A server MUST answer a frame for a stream it hasn't bound, and won't bind from that frame, with a reset sent back to the frame's sender.

### DC-0005-021 {#DC-0005-021}

A receiver MUST authenticate a packet whose frames it rejects for their routing before it acknowledges that packet.
