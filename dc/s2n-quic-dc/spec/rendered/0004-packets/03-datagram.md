## Datagram packets {#datagram}

A datagram packet carries one application message: its tag's flags, its credentials and wire version, the port at which its sender receives secret control packets, an optional packet number, optional routing information, the payload length, an optional application header with its length, the payload, and the authentication tag. Every byte before the payload is authenticated, and the payload is encrypted.

### DC-0004-030 {#DC-0004-030}

In a datagram packet's tag, bit 0x08 MUST be set exactly when the packet carries routing information.

### DC-0004-031 {#DC-0004-031}

In a datagram packet's tag, bit 0x04 MUST be set exactly when the packet carries a packet number.

### DC-0004-032 {#DC-0004-032}

In a datagram packet's tag, bit 0x02 MUST be set exactly when the packet carries an application header length.

### DC-0004-033 {#DC-0004-033}

Bit 0x01 of a datagram packet's tag MUST hold the key phase of the key that sealed the packet.

### DC-0004-034 {#DC-0004-034}

A datagram packet's fields MUST appear in this order: the tag, the credentials, the wire version, a 2-byte source control port, the packet number, the routing information, the payload length, the application header length, the application header, the payload and the authentication tag, each optional field only where its condition holds.

### DC-0004-035 {#DC-0004-035}

The packet number, the payload length and the application header length of a datagram packet MUST be variable-length integers.

### DC-0004-036 {#DC-0004-036}

Every datagram packet's payload MUST be encrypted.

### DC-0004-037 {#DC-0004-037}

A receiver MUST read a datagram packet that doesn't carry an application header length as having an empty application header.

### DC-0004-038 {#DC-0004-038}

The nonce of a datagram packet's seal MUST be its packet number, or zero where the packet carries none.

### DC-0004-039 {#DC-0004-039}

A sender MUST NOT seal another packet with a key that sealed a datagram packet without a packet number.

### DC-0004-040 {#DC-0004-040}

The associated data of a datagram packet's seal MUST be every byte from its tag to the end of its application header.

### DC-0004-041 {#DC-0004-041}

The routing information of a datagram packet MUST be a routing type of 11 followed by the source sender identifier, each a variable-length integer.

### DC-0004-042 {#DC-0004-042}

A receiver MUST NOT accept a datagram packet whose routing type is other than 11.

### DC-0004-043 {#DC-0004-043}

The source control port of a datagram packet MUST be the port at which its sender receives secret control packets.
