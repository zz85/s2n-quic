## Credentials {#credentials}

A credential identifies the keys that protected a packet. It holds a path secret identifier of 16 bytes and a key identifier, a variable-length integer that selects one set of keys derived from that path secret. Both endpoints of a path hold the same path secret, and the first bit of the identifier says which endpoint's copy it names.

### DC-0003-001 {#DC-0003-001}

A credential MUST consist of a 16-byte path secret identifier followed by a key identifier.

### DC-0003-002 {#DC-0003-002}

A credential MUST encode its key identifier as a QUIC variable-length integer, whose value is at most 2^62-1.

### DC-0003-003 {#DC-0003-003}

Decoding an encoded credential MUST yield the path secret identifier and the key identifier that were encoded.

### DC-0003-004 {#DC-0003-004}

The most significant bit of the first byte of a path secret identifier MUST be set where the server holds the secret and clear where the client holds it.

### DC-0003-005 {#DC-0003-005}

An endpoint MUST compute its peer's identifier of a shared path secret by inverting the most significant bit of the first byte of its own identifier.

### DC-0003-006 {#DC-0003-006}

An endpoint MUST derive each path secret identifier from its path secret, as the key derivation section states.
