## Accepting streams {#accepting}

A server registers acceptors on its endpoint, each under an identifier. Each incoming stream goes to the acceptor its identifier names, and waits in a receiver's queue until the application takes it. A stream that arrives before any receiver has polled for one or registered to take one is turned away as busy. The application clones a receiver to add workers, and drops every receiver to retire the acceptor.

### DC-0006-037 {#DC-0006-037}

A server MUST refuse to start where its endpoint and its handshake provider don't share one map of path secrets.

### DC-0006-038 {#DC-0006-038}

A server MUST deliver each incoming stream to the acceptor registered under the stream's acceptor identifier.

### DC-0006-039 {#DC-0006-039}

A server MUST refuse to register an acceptor under an identifier that is still registered, and report that the identifier is in use.

### DC-0006-040 {#DC-0006-040}

A server MUST let acceptors registered under different identifiers receive streams at the same time.

### DC-0006-041 {#DC-0006-041}

A server MUST reject a stream whose acceptor identifier doesn't name a registered acceptor.

### DC-0006-042 {#DC-0006-042}

Each receiver of an acceptor MUST hold its waiting streams in a queue whose capacity the application configures.

### DC-0006-043 {#DC-0006-043}

When a receiver's queue is full, the server MUST evict either the oldest or the newest waiting stream, as the application configures.

### DC-0006-044 {#DC-0006-044}

The server MUST reset each stream it evicts from a receiver's queue.

### DC-0006-045 {#DC-0006-045}

Where an acceptor has several receivers, the server SHOULD give each stream to the less loaded of two receivers it picks at random.

### DC-0006-046 {#DC-0006-046}

The server MUST NOT give streams to a receiver that hasn't yet polled for a stream or registered to take one.

### DC-0006-054 {#DC-0006-054}

Where an acceptor has receivers and none of them has polled for a stream or registered to take one, the server MUST reset an incoming stream to that acceptor as busy.

### DC-0006-047 {#DC-0006-047}

Once every receiver of an acceptor is dropped, the server MUST reject streams to the acceptor's identifier.

### DC-0006-056 {#DC-0006-056}

Once a stream to an acceptor finds every receiver of the acceptor dropped, the server MUST remove the acceptor's registration.
