## Packet form {#packet}

A tunneled datagram packet starts with a byte whose high four bits, 0100, mark a datagram packet, and whose low four bits, from the highest, say whether the packet carries routing information, whether it carries a packet number, whether it carries an application header, and the phase of the key that sealed it. A tunneled datagram carries a packet number, and carries neither routing information nor an application header. After the first byte come the credentials that identify the path secret and the key, the wire version, the sender's control port as a 16-bit number, the packet number and the payload's length as variable-length integers, the encrypted payload, and the authentication tag. Everything before the payload is the packet's header, which the tag authenticates and which isn't encrypted.

### DC-0009-007 {#DC-0009-007}

The first byte of a tunneled datagram packet MUST hold 0100 in its high four bits, and in its low four bits, from the highest, the routing information bit, the packet number bit, the application header bit and the key phase bit.

### DC-0009-008 {#DC-0009-008}

A tunneled datagram packet MUST NOT carry routing information, and its routing information bit is clear.

### DC-0009-009 {#DC-0009-009}

A tunneled datagram packet MUST carry its packet number, and its packet number bit is set.

### DC-0009-010 {#DC-0009-010}

A tunneled datagram packet MUST NOT carry an application header, and its application header bit is clear.

### DC-0009-011 {#DC-0009-011}

A tunneled datagram packet MUST carry, after its first byte and in this order, the credentials of the key that sealed it, the wire version, the sender's control port, the packet number, the payload's length, the payload and the authentication tag.

### DC-0009-012 {#DC-0009-012}

A sender MUST write wire version zero in each tunneled datagram packet it sends.

### DC-0009-013 {#DC-0009-013}

A sender MUST write as the packet's control port the source port of the control path its caller passes to the send.

### DC-0009-014 {#DC-0009-014}

A sender MUST encrypt the whole payload of each packet.

### DC-0009-028 {#DC-0009-028}

A sender MUST authenticate the whole header of each packet together with its payload.

### DC-0009-029 {#DC-0009-029}

A sender MUST use the packet's packet number as the nonce of its encryption.

### DC-0009-030 {#DC-0009-030}

A sender MUST write the phase of the key that seals a packet in the packet's key phase bit.
