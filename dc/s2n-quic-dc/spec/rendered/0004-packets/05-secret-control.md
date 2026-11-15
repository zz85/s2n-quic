## Secret control packets {#secret-control}

Four secret control packets report a problem with a path: an unknown path secret packet, a stale key packet, a replay detected packet and a queue reset packet. Each starts with its tag, a credential identifier or the credentials, and the wire version, and three of them can carry the identifier of the queue that is to receive them. Each ends with a 16-byte tag, and no secret control packet is longer than 64 bytes.

### DC-0004-056 {#DC-0004-056}

An unknown path secret packet's tag, with bit 0x04 cleared, MUST be 0x60.

### DC-0004-057 {#DC-0004-057}

A stale key packet's tag, with bit 0x04 cleared, MUST be 0x61.

### DC-0004-058 {#DC-0004-058}

A replay detected packet's tag, with bit 0x04 cleared, MUST be 0x62.

### DC-0004-059 {#DC-0004-059}

A queue reset packet's tag MUST be 0x67.

### DC-0004-060 {#DC-0004-060}

Bit 0x04 of the tag of an unknown path secret, stale key or replay detected packet MUST be set exactly when the packet carries a queue identifier.

### DC-0004-061 {#DC-0004-061}

A secret control packet MUST NOT be longer than 64 bytes.

### DC-0004-062 {#DC-0004-062}

The queue identifier, minimum key identifier, rejected key identifier, code and trigger of a secret control packet MUST be variable-length integers.

### DC-0004-063 {#DC-0004-063}

An unknown path secret packet MUST hold, in this order, its tag, the credential identifier, the wire version, the queue identifier where the tag's bit 0x04 is set, and the 16-byte stateless reset token of that credential identifier.

### DC-0004-064 {#DC-0004-064}

A receiver MUST accept an unknown path secret packet only where its last 16 bytes equal the stateless reset token of its credential identifier, compared in constant time.

### DC-0004-065 {#DC-0004-065}

A stale key packet MUST hold, in this order, its tag, the credential identifier, the wire version, the sender's queue identifier where the tag's bit 0x04 is set, the minimum key identifier its sender still accepts, and a 16-byte authentication tag.

### DC-0004-066 {#DC-0004-066}

A replay detected packet MUST hold, in this order, its tag, the credential identifier, the wire version, the sender's queue identifier where the tag's bit 0x04 is set, the key identifier its sender rejected as a replay, and a 16-byte authentication tag.

### DC-0004-067 {#DC-0004-067}

A queue reset packet MUST hold, in this order, its tag, the credentials, the wire version, the queue identifier, a code, the trigger and a 16-byte authentication tag.

### DC-0004-068 {#DC-0004-068}

The trigger of a queue reset packet MUST be 0 where an unroutable stream packet caused the reset and 1 where an unroutable control packet caused it.

### DC-0004-069 {#DC-0004-069}

A receiver MUST read a queue reset trigger other than 0 and 1 as 0.

### DC-0004-070 {#DC-0004-070}

The 16-byte authentication tag of a stale key, replay detected or queue reset packet MUST be a message authentication code, under the path's control secret, of every byte before it.
