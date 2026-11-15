## Sending {#sending}

A sender holds one key and the credentials of the path secret the key was derived from, and numbers the datagrams it seals from zero, one number for each send, a send that fails among them. A send that can't be framed, because the packet numbers are spent, the payload is too long for its length field or the caller's buffer can't hold the packet, fails without writing a packet. A caller can ask, before it sends, the most bytes the packet of a payload can take.

### DC-0009-001 {#DC-0009-001}

A sender MUST give each datagram it seals under a key a packet number that no earlier datagram under that key had, whichever of its callers sends it, and whether or not callers send at the same time.

### DC-0009-002 {#DC-0009-002}

A sender MUST take zero as the packet number of its first send, and one more than the number of the send before it as the number of each later send, whether or not the send before it failed.

### DC-0009-003 {#DC-0009-003}

A sender MUST NOT seal a datagram whose packet number is larger than a 62-bit variable-length integer holds, and fails that send instead.

### DC-0009-004 {#DC-0009-004}

A sender MUST fail a send whose payload is longer than the packet's payload length field can state.

### DC-0009-005 {#DC-0009-005}

A sender MUST fail a send, without writing to the caller's buffer, where that buffer is shorter than the packet the send would write.

### DC-0009-006 {#DC-0009-006}

A sender MUST answer a caller, before any send, the most bytes the packet of a payload of a given length can take, or that a payload of that length is too long to send.
