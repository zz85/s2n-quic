## Binding server streams {#binding}

A client opens a stream by sending frames marked as opening frames: a data frame, a message frame or a reset. Each opening frame carries the acceptor that is to receive the new stream and the credit tier the stream is to use. The server binds the stream on the first opening frame that arrives; once the server confirms the binding, the client sends later frames without the opening fields. A frame for a free slot whose binding is newer than any the slot has held, which the server can't bind, is for a stream the server never held: the server answers it with a reset that carries the stream-not-found error code and the frame's binding. A late frame of a stream the server has freed, and a frame of a newer stream that waits for a slot another stream still holds, are dropped without an answer.

### DC-0005-017 {#DC-0005-017}

A server MUST bind a new stream on the first opening frame for it that arrives, whether that frame is a data frame, a message frame or a reset.

### DC-0005-018 {#DC-0005-018}

A client MUST carry the same acceptor and the same credit tier on every opening frame of a stream.

### DC-0005-019 {#DC-0005-019}

A server MUST NOT bind a stream again when an opening frame arrives for a stream it has already bound.

### DC-0005-020 {#DC-0005-020}

A server MUST answer a frame for a free slot whose binding identifier is newer than the slot's last binding, and that it won't bind from that frame, with a reset sent back to the frame's sender.

### DC-0005-021 {#DC-0005-021}

A receiver MUST authenticate a packet whose frames it rejects for their routing before it acknowledges that packet.

### DC-0005-053 {#DC-0005-053}

The reset that answers a frame under DC-0005-020 MUST carry the stream-not-found error code.

### DC-0005-054 {#DC-0005-054}

The reset that answers a frame under DC-0005-020 MUST carry the binding identifier of the frame it answers.

### DC-0005-055 {#DC-0005-055}

A server MUST NOT answer with a reset a frame for a slot that a stream holds, or a frame whose binding identifier isn't newer than the last binding of its free slot.

### DC-0005-056 {#DC-0005-056}

A client MUST NOT answer with a reset a reset that arrives for a stream it doesn't hold.

### DC-0005-057 {#DC-0005-057}

An endpoint MUST NOT carry the stream-not-found error code on a reset other than one that answers a frame under DC-0005-020.

### DC-0005-058 {#DC-0005-058}

A client MUST NOT answer with a reset a frame of any kind that arrives for a stream it doesn't hold, a control frame among them.
