## Establishing a path secret {#handshake}

An endpoint establishes a path secret with a peer by a TLS 1.3 handshake carried over QUIC. Both endpoints export the same secret from the handshake, exchange stateless reset tokens for it, and record it in their secret maps. RFC 0003 specifies the keys derived from a path secret; this section states how the path secret itself is obtained and which parameters of the handshake it carries.

### DC-0002-001 {#DC-0002-001}

An endpoint MUST obtain each path secret from the exporter of its TLS 1.3 handshake with the peer, with the label `EXPERIMENTAL EXPORTER s2n-quic-dc` and an empty context.

### DC-0002-002 {#DC-0002-002}

An endpoint MUST NOT complete a handshake whose negotiated cipher suite is neither TLS_AES_128_GCM_SHA256 nor TLS_AES_256_GCM_SHA384.

### DC-0002-003 {#DC-0002-003}

An endpoint MUST bind each path secret to the cipher suite and the protocol version negotiated in its handshake and to the endpoint's role in that handshake, client or server.

### DC-0002-004 {#DC-0002-004}

An endpoint MUST give its peer one stateless reset token for each path secret during the handshake.

### DC-0002-074 {#DC-0002-074}

An endpoint MUST compute each stateless reset token as a keyed message authentication code over the path secret's identifier, with a key that only the endpoint holds.

### DC-0002-005 {#DC-0002-005}

An endpoint MUST keep with each path secret the stateless reset token its peer gave for it.

### DC-0002-006 {#DC-0002-006}

An endpoint SHOULD keep the key it computes stateless reset tokens with across its restarts.

### DC-0002-007 {#DC-0002-007}

An endpoint MAY refuse a handshake where its application rejects the peer information the peer advertised, closing the handshake with an application error.

### DC-0002-008 {#DC-0002-008}

An endpoint MUST NOT select a path secret for new flows to a peer before the handshake that produced it has completed.

### DC-0002-009 {#DC-0002-009}

An endpoint MUST NOT send a datagram under a path secret that is larger than the smaller of the peer's advertised maximum datagram size and the endpoint's own maximum datagram size.

### DC-0002-010 {#DC-0002-010}

An endpoint SHOULD raise the maximum datagram size of a path secret to the size that path MTU discovery during the handshake confirms.

### DC-0002-011 {#DC-0002-011}

An endpoint SHOULD fail a handshake whose confirmation hasn't finished 10 seconds after its connection is established.

### DC-0002-012 {#DC-0002-012}

A server MUST keep a handshake connection open until the client closes it or the bound of DC-0002-011 passes.
