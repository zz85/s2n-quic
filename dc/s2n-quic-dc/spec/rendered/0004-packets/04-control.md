## Control packets {#control}

A control packet carries control frames from a stream's receiver to its sender: its tag's flags, its credentials and wire version, the stream identifier where it answers one stream, an optional source queue identifier, the packet number, the control data length, optional routing information, the control data and the authentication tag. The control data isn't encrypted; the authentication tag covers the whole packet.

### DC-0004-044 {#DC-0004-044}

In a control packet's tag, bit 0x08 MUST be set exactly when the packet carries a source queue identifier.

### DC-0004-045 {#DC-0004-045}

In a control packet's tag, bit 0x04 MUST be set exactly when the packet carries the identifier of the stream it answers.

### DC-0004-046 {#DC-0004-046}

In a control packet's tag, bit 0x02 MUST be set exactly when the packet carries routing information.

### DC-0004-047 {#DC-0004-047}

A sender MUST set bit 0x01 of a control packet's tag to zero.

### DC-0004-048 {#DC-0004-048}

A control packet's fields MUST appear in this order: the tag, the credentials, the wire version, the stream identifier, the source queue identifier, the packet number, the control data length, the routing information, the control data and the authentication tag, each optional field only where its condition holds.

### DC-0004-049 {#DC-0004-049}

The stream identifier, source queue identifier, packet number and control data length of a control packet MUST be variable-length integers, the stream identifier encoded as a stream packet's is.

### DC-0004-050 {#DC-0004-050}

The control data of a control packet MUST be a sequence of QUIC frames.

### DC-0004-051 {#DC-0004-051}

A sender MUST NOT encrypt a control packet's control data.

### DC-0004-052 {#DC-0004-052}

A control packet's authentication tag MUST cover every byte from the packet's tag to the end of its control data.

### DC-0004-053 {#DC-0004-053}

The nonce of a control packet's authentication tag MUST be its packet number.

### DC-0004-054 {#DC-0004-054}

The routing information of a control packet MUST be a routing type of 0 followed by the source sender identifier and the destination sender identifier, each a variable-length integer.

### DC-0004-055 {#DC-0004-055}

A receiver MUST NOT accept a control packet whose routing type is other than 0.
