## Receiving datagrams {#receive}

A receiver reads datagrams from a datagram socket and hands each one, with its sender address and its explicit congestion notification marking, to the next stage. One read may return several datagrams that the network stack coalesced, each of the same segment size except a shorter last one. A receiver reads its socket in one of three ways: on the socket's readiness, by polling it in a busy-poll worker, or through a completion queue that the operating system fills. The first two read the socket again after a read of zero bytes, and the third hands the empty datagram on to dispatch, which drops it because it doesn't decode. Whichever way it reads, the same packets reach the handlers.

### DC-0010-001 {#DC-0010-001}

A receiver MUST deliver each datagram it reads with the address of the datagram's sender and the explicit congestion notification marking the network set on it.

### DC-0010-002 {#DC-0010-002}

Where one read returns several coalesced datagrams with a segment size, the receiver MUST deliver them as separate datagrams, each of that size except a shorter last one.

### DC-0010-003 {#DC-0010-003}

A read that returns zero bytes MUST NOT reach a packet handler as a datagram.

### DC-0010-004 {#DC-0010-004}

A receiver that finds its socket empty MUST yield until the socket signals that it is readable, or until its next poll in busy-poll mode.

### DC-0010-005 {#DC-0010-005}

The mechanism a receiver uses to read its socket MUST NOT change which packets reach a handler, or the payload, the sender address or the congestion marking of each one.

### DC-0010-006 {#DC-0010-006}

A receiver SHOULD deliver each payload in the buffer the operating system wrote it into, without copying it into another buffer.

### DC-0010-007 {#DC-0010-007}

A receiver MUST release its readiness registration for a socket before the socket is closed.

### DC-0010-008 {#DC-0010-008}

A socket SHOULD be read by the worker that registered it for readiness, so that its wake-ups run on that worker.
