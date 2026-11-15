## The secret map {#secret-map}

An endpoint keeps each path secret it holds in its secret map, which a packet's path secret identifier and a peer's address both index. A path secret enters the map when its handshake exchanges stateless reset tokens, and becomes the one chosen for the peer's address when the handshake completes.

### DC-0002-024 {#DC-0002-024}

An endpoint MUST keep the path secrets for which it was the client apart from those for which it was the server, so that one peer address can hold a path secret in each role.

### DC-0002-025 {#DC-0002-025}

An endpoint MUST find the path secret for a received packet by the path secret identifier the packet carries.

### DC-0002-026 {#DC-0002-026}

An endpoint MUST choose the path secret for a new flow to a peer by the peer's address, among the path secrets for which it was the client.

### DC-0002-027 {#DC-0002-027}

When a handshake completes for a peer address and role for which the endpoint already holds a path secret, the endpoint MUST choose the new path secret for the new flows of that address and role.

### DC-0002-028 {#DC-0002-028}

An endpoint MUST retire the earlier path secret that a newer one replaces for a peer address and role, keeping it for the packets that still use it until it is evicted.

### DC-0002-029 {#DC-0002-029}

An endpoint MUST hold at most a configured number of path secrets.

### DC-0002-030 {#DC-0002-030}

When a new path secret takes the count past the configured capacity, the endpoint MUST evict the path secret it recorded earliest.

### DC-0002-031 {#DC-0002-031}

An endpoint MUST NOT remove a newer path secret from the address index when it evicts an earlier path secret for the same peer address.

### DC-0002-032 {#DC-0002-032}

An endpoint MUST NOT hold two path secrets with the same identifier.
