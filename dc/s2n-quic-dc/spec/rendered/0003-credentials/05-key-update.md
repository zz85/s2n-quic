## Key update {#key-update}

Each direction of a bidirectional stream holds a key-update secret beside its key. An update expands that secret into the next key-update secret, the next key and the next IV. A one-bit key phase in each packet says which key sealed it, and alternates at each update.

### DC-0003-036 {#DC-0003-036}

A key update MUST expand the current key-update secret, used as the pseudorandom key, with an info input of the output length followed by the label `ku`.

### DC-0003-037 {#DC-0003-037}

The output of a key update MUST be read, in order, as the next key-update secret and the next key, each of the ciphersuite's key length, and the next 12-byte IV.

### DC-0003-038 {#DC-0003-038}

A sender MUST update its sealing key once it has sealed 2^23 - 2^16 packets with it.

### DC-0003-039 {#DC-0003-039}

A sender MUST mark each packet with the key phase, 0 or 1, of the key that sealed it.

### DC-0003-050 {#DC-0003-050}

A sender MUST switch its key phase to the other value at each key update.

### DC-0003-040 {#DC-0003-040}

A receiver MUST keep an opening key for each of the two key phases.

### DC-0003-041 {#DC-0003-041}

A receiver that opens a packet of the phase it doesn't expect MUST replace the key of the phase it expected with the next key of its chain.

### DC-0003-051 {#DC-0003-051}

After that replacement, a receiver MUST expect the key phase of the packet that it opened.

### DC-0003-042 {#DC-0003-042}

An endpoint MUST NOT open a packet of key phase 1 with a key that has no key-update secret.
