## Trust model {#trust}

Each peer trusts the other as far as the handshake between them authenticated it, with the credentials that the deployment's handshake configuration asks for: a server that doesn't ask for the client's credentials derives path secrets with any client that reaches it. That handshake's session yields the path secret, from which both peers derive the keys of each later stream and datagram, and the stateless reset tokens each sends the other. A packet that doesn't authenticate under a key, or against a stateless reset token, derived from a path secret comes from a host the system doesn't trust.

### DC-0001-014 {#DC-0001-014}

The system MUST derive a path secret only from a completed handshake with the peer that shares it.

### DC-0001-015 {#DC-0001-015}

A deployment SHOULD configure the handshake to authenticate both peers.

### DC-0001-016 {#DC-0001-016}

The system MUST NOT accept the data of a packet of the dc protocol that doesn't authenticate under a key, or against a stateless reset token, derived from a path secret it holds.

### DC-0001-017 {#DC-0001-017}

Once the handshake's path secret is derived, a client MUST send the server the stateless reset tokens it derives from that secret.

### DC-0001-018 {#DC-0001-018}

A server MUST send the client the stateless reset tokens it derives from the path secret once it has received the client's tokens.

### DC-0001-019 {#DC-0001-019}

A client MUST treat the dc handshake as complete once it receives the server's stateless reset tokens.

### DC-0001-020 {#DC-0001-020}

A server MUST treat the dc handshake as complete once the client acknowledges the frame that carries the server's stateless reset tokens.
