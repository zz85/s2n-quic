## Goals {#goals}

s2n-quic-dc is a transport for peers inside a datacenter. Two hosts run one authenticated handshake, which establishes a secret for the path between them, and each stream and datagram they exchange afterwards is protected by keys derived from that secret, without a handshake of its own. The later RFCs of this specification state the path secrets, the keys, the wire format, the streams, recovery and congestion control, scheduling, datagrams, sockets and the runtime.

### DC-0001-001 {#DC-0001-001}

The system MUST protect each packet of the dc protocol's streams and datagrams with a key derived from the path secret the two peers share.

### DC-0001-002 {#DC-0001-002}

The system MUST let a peer open a stream or send a datagram on a path whose secret it already holds without running a new handshake.

### DC-0001-003 {#DC-0001-003}

The system MUST NOT send application data to a peer in a packet of the dc protocol before it holds a path secret shared with that peer.

### DC-0001-004 {#DC-0001-004}

The system MUST carry reliable, ordered streams between peers that share a path secret.

### DC-0001-005 {#DC-0001-005}

The system MUST carry unreliable datagrams between peers that share a path secret.
