## Handling keys and opened data {#handling}

Some keys protect one packet alone, such as the key of a single datagram. The replay check that RFC 0002 states runs once for each key, on the first packet the key opens, and the payload of that packet is released only after the check accepts it.

### DC-0003-043 {#DC-0003-043}

An endpoint MUST overwrite a path secret's export secret with zeros when it drops the path secret.

### DC-0003-044 {#DC-0003-044}

A single-use key MUST NOT seal more than one packet.

### DC-0003-045 {#DC-0003-045}

A single-use key MUST NOT open more than one packet.

### DC-0003-046 {#DC-0003-046}

A receiver MUST run a key's replay check on the first packet the key opens, after the AEAD verifies that packet's tag and before it releases the payload.

### DC-0003-047 {#DC-0003-047}

A receiver whose replay check refuses an opened packet MUST overwrite the packet's decrypted payload with zeros.

### DC-0003-048 {#DC-0003-048}

An endpoint MUST NOT run the replay check on the opening key of a bidirectional stream that it initiated.
