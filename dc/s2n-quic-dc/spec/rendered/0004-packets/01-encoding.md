## Common encoding {#encoding}

Every packet starts with a one-byte tag, whose value names the packet's kind and whose low bits carry that kind's flags. Integers of fixed width are sent most significant byte first, and every other integer is a variable-length integer. Most packets then carry the credentials that select the keys of the path, and a one-byte wire version.

### DC-0004-001 {#DC-0004-001}

Each fixed-width integer field MUST be encoded most significant byte first.

### DC-0004-002 {#DC-0004-002}

Each field that this RFC calls a variable-length integer MUST be encoded as a QUIC variable-length integer, whose first byte's two most significant bits give its length of 1, 2, 4 or 8 bytes.

### DC-0004-003 {#DC-0004-003}

The first byte of every packet MUST be its tag, which names the packet's kind: 0x00 to 0x3F a stream packet, 0x40 to 0x4F a datagram packet, 0x50 to 0x5F a control packet, and 0x60, 0x61, 0x62, 0x64, 0x65, 0x66 and 0x67 a secret control packet.

### DC-0004-004 {#DC-0004-004}

A receiver MUST NOT accept a packet whose tag has its most significant bit set.

### DC-0004-005 {#DC-0004-005}

A receiver MUST NOT accept a packet whose tag is 0x63 or lies between 0x68 and 0x7F.

### DC-0004-006 {#DC-0004-006}

The wire version field of a packet MUST be one byte holding 0.

### DC-0004-007 {#DC-0004-007}

A receiver MUST NOT accept a packet whose wire version is other than 0.

### DC-0004-008 {#DC-0004-008}

A credential identifier MUST be 16 bytes long.

### DC-0004-009 {#DC-0004-009}

The most significant bit of a credential identifier's first byte MUST be set where the identifier names the server endpoint's side of a path, and clear where it names the client endpoint's side.

### DC-0004-010 {#DC-0004-010}

The credentials field MUST be the 16-byte credential identifier followed by the key identifier as a variable-length integer.

### DC-0004-011 {#DC-0004-011}

Each stream, datagram and control packet MUST end with an authentication tag at least 16 bytes long, of the length the path's cipher produces.
