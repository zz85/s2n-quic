## Endpoint counters {#endpoint-counters}

The receive path counts each datagram by what happened to it, each failure by its cause, each frame by its type and each datagram by its congestion marking, and it times each stage of its work.

### DC-0013-039 {#DC-0013-039}

The receive path MUST count each datagram by its outcome: dispatched to a stream, dispatched to a control queue, refused because no acceptor was available, the acceptor was closed or no free slot was available, or dropped for an unallocated, half-closed, stale or future binding.

### DC-0013-040 {#DC-0013-040}

The receive path MUST count each processing failure by its cause: an unknown peer, a decryption failure, a duplicate packet, a stale key or a missing sender identifier.

### DC-0013-041 {#DC-0013-041}

The receive path MUST count received datagrams by their explicit congestion notification marking.

### DC-0013-042 {#DC-0013-042}

The receive path MUST count each frame it receives by the frame's type.

### DC-0013-043 {#DC-0013-043}

The receive path MUST record the time it spends looking up the peer, decrypting and dispatching each datagram.

### DC-0013-044 {#DC-0013-044}

The receive path SHOULD count the hits and the misses of its peer cache.

### DC-0013-048 {#DC-0013-048}

The receive path MUST record the size and the frame count of each datagram it receives.
