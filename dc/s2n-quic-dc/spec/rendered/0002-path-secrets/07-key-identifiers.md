## Key identifiers {#key-identifiers}

A sender gives each packet key it derives from a path secret a key identifier, which the packets sealed under that key carry. The receiver's stale key and replay detected packets tell the sender which key identifiers it can no longer accept.

### DC-0002-052 {#DC-0002-052}

An endpoint MUST give each sending key it derives from a path secret the key identifier one greater than the last one it gave under that path secret, starting at 0, except where DC-0002-055 or DC-0002-056 raises it.

### DC-0002-053 {#DC-0002-053}

An endpoint MUST NOT use one key identifier for two sending keys under one path secret.

### DC-0002-054 {#DC-0002-054}

An endpoint MUST NOT use a key identifier of 2^62 - 1 or greater.

### DC-0002-055 {#DC-0002-055}

On an authenticated stale key packet, an endpoint MUST raise the next key identifier it uses under the path secret the packet names to at least the minimum key identifier the packet carries.

### DC-0002-056 {#DC-0002-056}

On an authenticated replay detected packet, an endpoint MUST raise the next key identifier it uses under the path secret the packet names to at least one more than the rejected key identifier the packet carries.

### DC-0002-057 {#DC-0002-057}

An endpoint MUST request a handshake with the peer when it accepts a replay detected packet.
