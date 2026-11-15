## Stream packets {#stream}

A stream packet carries part of a stream: its tag's flags, its credentials and wire version, two reserved bytes, the stream identifier, an optional source queue identifier, the packet number, a retransmission offset on a reliable stream, the next control packet number the sender expects, the stream offset, an optional final offset, the lengths of its parts, an optional application header, optional control data, the payload, and the authentication tag. The payload is encrypted, and every byte before it is authenticated. A reliable stream retransmits a packet with the packet number and nonce of the original, and records the retransmission's own number as an offset from it.

### DC-0004-012 {#DC-0004-012}

In a stream packet's tag, bit 0x20 MUST be set exactly when the packet carries a source queue identifier.

### DC-0004-013 {#DC-0004-013}

In a stream packet's tag, bit 0x10 MUST be set exactly when the packet belongs to the recovery packet space.

### DC-0004-014 {#DC-0004-014}

In a stream packet's tag, bit 0x08 MUST be set exactly when the packet carries control data.

### DC-0004-015 {#DC-0004-015}

In a stream packet's tag, bit 0x04 MUST be set exactly when the packet carries a final offset.

### DC-0004-016 {#DC-0004-016}

In a stream packet's tag, bit 0x02 MUST be set exactly when the packet carries an application header.

### DC-0004-017 {#DC-0004-017}

Bit 0x01 of a stream packet's tag MUST hold the key phase of the key that sealed the packet.

### DC-0004-018 {#DC-0004-018}

A stream packet's fields MUST appear in this order: the tag, the credentials, the wire version, two reserved bytes, the stream identifier, the source queue identifier, the packet number, the retransmission offset, the next expected control packet number, the stream offset, the final offset, the control data length, the payload length, the application header length, the application header, the control data, the payload and the authentication tag, each optional field only where its condition holds.

### DC-0004-019 {#DC-0004-019}

A sender MUST set the two reserved bytes that follow a stream packet's wire version to zero.

### DC-0004-020 {#DC-0004-020}

A stream identifier MUST be one variable-length integer whose value is the stream's queue identifier multiplied by 4, plus 2 where the stream is reliable, plus 1 where it is bidirectional.

### DC-0004-021 {#DC-0004-021}

The source queue identifier, packet number, next expected control packet number, stream offset, final offset and every length of a stream packet MUST be variable-length integers.

### DC-0004-022 {#DC-0004-022}

A packet of a reliable stream MUST carry a 4-byte retransmission offset after its packet number, whose value added to the packet number gives the number of this transmission.

### DC-0004-023 {#DC-0004-023}

A receiver MUST NOT accept a stream packet whose packet number plus retransmission offset is larger than the largest value a variable-length integer holds.

### DC-0004-024 {#DC-0004-024}

The nonce of a stream packet's seal MUST be its packet number.

### DC-0004-025 {#DC-0004-025}

The associated data of a stream packet's seal MUST be every byte from its tag to the end of its control data.

### DC-0004-026 {#DC-0004-026}

A stream packet in the recovery packet space that isn't a retransmission MUST NOT carry a payload.

### DC-0004-027 {#DC-0004-027}

A sender MUST NOT retransmit a packet of a stream that isn't reliable.

### DC-0004-028 {#DC-0004-028}

Before a receiver opens a retransmitted stream packet, it MUST clear the tag's recovery bit, set the four retransmission offset bytes to zero and remove the retransmission's change to the authentication tag, so that it checks the header the first transmission sealed.

### DC-0004-029 {#DC-0004-029}

A receiver MUST take a stream packet as its stream's last where the packet's final offset equals its stream offset plus its payload length.
