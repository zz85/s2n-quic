## Accepting streams {#accepting}

A server registers acceptors on its endpoint, each under an identifier. Each incoming stream goes to the acceptor its identifier names, and waits in a receiver's queue until the application takes it. A stream that arrives before any receiver has polled for one or registered to take one is turned away as busy. The application clones a receiver to add workers, and drops every receiver to retire the acceptor. A queue slot is the room the server holds for one stream of one client: the client opens each stream on a slot it holds, each write of the stream names that slot until the server confirms the stream, and the stream keeps its slot until the application has finished with it. The server then returns the freed slot to the client in a frame that names the slot, and the client may open another stream on it. The server starts each client with one slot where the memory that holds waiting streams has room for it and no client waits for its first slot, and otherwise with none until that memory has room for one, which goes to the client that has waited longest, ahead of any grant that grows; it then grows the client's grant, up to doubling it, while its receivers take that client's streams as fast as they arrive, and holds it while streams wait or halves it by keeping freed slots. The server takes room for each slot from the memory that holds waiting streams when it grants the slot, so a stream on a granted slot always has room, and returns that room when the slot leaves the grant. A slot's room is the memory for one stream's state and for the bytes the client may send on that stream before the application takes it: up to the read half's starting limit on what the client may send, since the read half raises that limit only as the application consumes data. While that memory refuses room for a slot, no grant grows, the server takes back each freed slot that no withdrawal frame names, and it withdraws each granted slot that holds no stream, in a frame that names the slot, keeping in each case a client's last slot; a withdrawn slot leaves the grant once it holds no stream of the client and the client confirms the withdrawal or acknowledges a packet that carried it. The server never returns a withdrawn slot to the client, and resets a later stream on it with an error that tells the client to open the stream on another slot, which the client then does: such a stream can be one the client opened before it read the withdrawal, whose first write reached the server after the client's acknowledgement.

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

### DC-0006-057 {#DC-0006-057}

Each receiver of an acceptor MUST keep every stream that waits for it until the application takes the stream or drops the receiver.

### DC-0006-058 {#DC-0006-058}

The server MUST NOT evict a waiting stream from a receiver's queue to make room for a new stream.

### DC-0006-059 {#DC-0006-059}

The server MUST NOT return a stream's queue slot to the client, or take it out of the client's grant, until the application has finished with the stream.

### DC-0006-060 {#DC-0006-060}

The server MUST set the number of queue slots each client may hold for streams to that server.

### DC-0006-061 {#DC-0006-061}

At the handshake, the server MUST advertise to each client a starting grant of one queue slot where the memory that holds waiting streams gives room for that slot and no client waits for its first slot.

### DC-0006-062 {#DC-0006-062}

The server MUST derive each client's grant of queue slots from how its acceptors' receivers take that client's streams and from the room that the memory holding waiting streams gives for slots, and not from a fixed number.

### DC-0006-063 {#DC-0006-063}

Where a receiver takes a stream of a client that holds every slot of its grant and no stream of that client waits in any receiver's queue after the take, the server MUST grow that client's grant by as many slots as the memory that holds waiting streams gives room for, up to doubling the grant.

### DC-0006-064 {#DC-0006-064}

The server MUST NOT grow a client's grant while streams of that client wait in a receiver's queue.

### DC-0006-065 {#DC-0006-065}

While streams of a client wait in a receiver's queue, the server MAY shrink that client's grant by keeping freed slots that no withdrawal frame names instead of returning them to the client, down to half the grant the client held when its streams began to wait, or further where DC-0006-073 asks.

### DC-0006-066 {#DC-0006-066}

The server MUST tell a client of each slot it adds to the client's grant by naming that slot in the frame that returns freed slots to the client.

### DC-0006-067 {#DC-0006-067}

The client MUST take a slot that the server names as free and that the client hasn't held as a new slot of its grant.

### DC-0006-068 {#DC-0006-068}

The client MUST NOT open a stream on a queue slot that the server hasn't granted it.

### DC-0006-069 {#DC-0006-069}

The server MUST refuse a stream whose first write to reach the server names a queue slot that the server doesn't grant that client.

### DC-0006-070 {#DC-0006-070}

Once the server has granted a client its first queue slot, the server MUST keep that client's grant at one slot or more.

### DC-0006-071 {#DC-0006-071}

The server MUST refuse a stream whose first write to reach the server names a queue slot that holds another stream the server hasn't freed.

### DC-0006-072 {#DC-0006-072}

The server MUST NOT grant a client a queue slot that the memory holding waiting streams hasn't given room for.

### DC-0006-073 {#DC-0006-073}

While the memory that holds waiting streams refuses room for a slot, the server MUST take each freed slot that no withdrawal frame names out of its client's grant instead of returning it, except the last slot of that client's grant that no withdrawal frame names.

### DC-0006-074 {#DC-0006-074}

The server MUST keep the room it took for a granted slot until the slot leaves the client's grant.

### DC-0006-075 {#DC-0006-075}

Where the memory that holds waiting streams refuses room for a client's first queue slot at the handshake, or another client waits for its first slot, the server MUST advertise to that client a starting grant of no slots.

### DC-0006-076 {#DC-0006-076}

While a client waits for its first queue slot, the server MUST keep one request for a slot's room open with the memory that holds waiting streams, for the client that has waited longest.

### DC-0006-077 {#DC-0006-077}

The server MUST return to the memory that holds waiting streams the room of each slot that leaves a client's grant.

### DC-0006-078 {#DC-0006-078}

The server MUST hand each room that the memory holding waiting streams frees to the client that has waited longest for its first queue slot, before it grows any grant under DC-0006-063.

### DC-0006-079 {#DC-0006-079}

While the memory that holds waiting streams refuses room for a slot, the server MUST withdraw from each client's grant each granted slot that holds no stream, except the last slot of that client's grant that no withdrawal frame names.

### DC-0006-080 {#DC-0006-080}

The server MUST tell a client of each slot it withdraws by naming that slot in a frame that withdraws slots.

### DC-0006-081 {#DC-0006-081}

The client MUST NOT open a stream on a queue slot that a withdrawal frame it has received names.

### DC-0006-082 {#DC-0006-082}

The client MUST confirm to the server the withdrawal of each slot that a withdrawal frame names, once that slot holds no stream of the client, with the retransmission the frame that withdraws slots has.

### DC-0006-083 {#DC-0006-083}

The server MUST keep each slot it withdraws in the client's grant until that slot holds no stream of the client and the client has confirmed the slot's withdrawal or acknowledged a packet that carried a withdrawal frame naming the slot.

### DC-0006-088 {#DC-0006-088}

The server MUST take a slot it has withdrawn out of the client's grant once that slot holds no stream of the client and the client has confirmed the slot's withdrawal or acknowledged a packet that carried a withdrawal frame naming the slot.

### DC-0006-084 {#DC-0006-084}

The server MUST NOT return to the client a slot it has withdrawn.

### DC-0006-085 {#DC-0006-085}

Where the server refuses under DC-0006-069 a stream whose first write names a slot whose withdrawal has ended, the server MUST reset that stream with an error that tells the client to open the stream on another slot.

### DC-0006-086 {#DC-0006-086}

The client MUST open again on another granted slot a stream that the server resets with the error of DC-0006-085.

### DC-0006-087 {#DC-0006-087}

The client MUST keep each byte the application has written on a stream until the server confirms the stream.

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
