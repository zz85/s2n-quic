## Key derivation {#derivation}

Every key of a path is derived with the HKDF-Expand function of the path's ciphersuite, with the export secret as the pseudorandom key and no extract step. The info input of each expansion is a sequence of fields. A space-prefixed label is the byte 0x20 followed by the ASCII letters of its word, and every label below is space-prefixed. A role label is the label `client` or the label `server`. A key identifier in an info input is written as 8 bytes, most significant byte first. Where an expansion yields several values, they are read from its output in the order listed, with the key-update secrets, whose disclosure would expose every later key, first.

### DC-0003-012 {#DC-0003-012}

Each key derivation MUST use the HKDF-Expand function of the path's ciphersuite with the export secret as its pseudorandom key, without an HKDF-Extract step.

### DC-0003-013 {#DC-0003-013}

The info input of each derivation other than the path secret identifier's MUST begin with the length of the derivation's output in bytes, as a 2-byte big-endian integer.

### DC-0003-014 {#DC-0003-014}

A path secret identifier MUST be the 16-byte output of an expansion whose info input is the single byte 16 followed by the label `pid`, with its role bit then set as the credentials section states.

### DC-0003-015 {#DC-0003-015}

The info input of a bidirectional stream's application keys MUST be the output length, the label `bidi`, the role label of the endpoint that initiated the stream, the label `app` and the key identifier, in that order.

### DC-0003-016 {#DC-0003-016}

The output of a bidirectional stream's application key derivation MUST be read, in order, as the client's key-update secret, the server's key-update secret, the client's key, the server's key, the client's 12-byte IV and the server's 12-byte IV, each secret and key of the ciphersuite's key length.

### DC-0003-017 {#DC-0003-017}

Where a derivation yields a client's value and a server's value, the client MUST seal with the client's value and open with the server's value.

### DC-0003-018 {#DC-0003-018}

Where a derivation yields a client's value and a server's value, the server MUST seal with the server's value and open with the client's value.

### DC-0003-019 {#DC-0003-019}

The info input of a bidirectional stream's control keys MUST be the output length, the label `bidi`, the role label of the endpoint that initiated the stream, the label `ctl` and the key identifier, in that order.

### DC-0003-020 {#DC-0003-020}

The output of a bidirectional stream's control key derivation MUST be read as the client's HMAC key followed by the server's HMAC key, each as long as the block length of the ciphersuite's hash function.

### DC-0003-021 {#DC-0003-021}

The info input of a unidirectional application key MUST be the output length, the label `uni`, the role label of the sending endpoint and the key identifier, in that order.

### DC-0003-022 {#DC-0003-022}

The output of a unidirectional application key derivation MUST be read as the key, of the ciphersuite's key length, followed by a 12-byte IV.

### DC-0003-023 {#DC-0003-023}

The info input of a secret-control key MUST be the output length, the label `ctl` and the role label of the sending endpoint, without a key identifier.

### DC-0003-024 {#DC-0003-024}

The output of a secret-control key derivation MUST be one HMAC key as long as the block length of the ciphersuite's hash function.

### DC-0003-025 {#DC-0003-025}

Two derivations for different purposes, roles or key identifiers MUST NOT use the same info input.
