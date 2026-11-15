## Deployment model {#deployment}

Each host runs an endpoint that accepts handshakes and an endpoint that carries data. The host that opens a path is its client, and the host that accepts it is its server; one host can be both, toward different peers. The handshake runs between the two hosts' handshake addresses. During it, each peer can describe itself and advertise the addresses on which it receives data packets, and once it completes, the peers exchange data on those addresses. A peer that doesn't send either description is treated as an older peer that didn't send it.

### DC-0001-006 {#DC-0001-006}

The system MUST run each handshake as a QUIC connection over UDP whose cryptographic handshake is TLS 1.3.

### DC-0001-007 {#DC-0001-007}

A peer MAY advertise in the handshake the addresses on which it receives data packets, which can differ from its handshake address.

### DC-0001-008 {#DC-0001-008}

A peer MAY send data packets of the dc protocol to the addresses the other peer advertised as soon as the QUIC handshake completes.

### DC-0001-009 {#DC-0001-009}

A peer MUST carry its peer information in the transport parameter 0xdc0003.

### DC-0001-010 {#DC-0001-010}

A peer MUST carry the addresses it advertises in the transport parameter 0xdc0004.

### DC-0001-011 {#DC-0001-011}

A peer MUST complete a handshake with a peer that sends neither its peer information nor its data addresses.

### DC-0001-012 {#DC-0001-012}

An endpoint that runs the dc protocol MUST pass each short-header packet that no connection accepts to its handling of path secret control packets.

### DC-0001-013 {#DC-0001-013}

An endpoint MAY decline to use the dc protocol on a path after the dc version is negotiated.
