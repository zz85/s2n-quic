## Secret control packets {#control-packets}

There are three secret control packets. A stale key packet carries one more than the highest key identifier its sender has seen, or 0 where it has seen none; a replay detected packet carries a key identifier its sender rejected; both are authenticated with the control key of the path secret they name, which RFC 0003 derives. An unknown path secret packet tells a peer that its sender doesn't hold the path secret a packet named, and is authenticated with the stateless reset token its sender gave for that path secret. RFC 0004 specifies their wire format.

### DC-0002-065 {#DC-0002-065}

An endpoint MUST answer packets that name a path secret it doesn't hold with unknown path secret packets authenticated with the stateless reset token the endpoint computes for that path secret's identifier.

### DC-0002-075 {#DC-0002-075}

An endpoint MUST send its unknown path secret packets at a rate that stays within a limit the endpoint configures.

### DC-0002-066 {#DC-0002-066}

An endpoint MUST authenticate each stale key and replay detected packet it sends with the control key of the path secret the packet names.

### DC-0002-067 {#DC-0002-067}

An endpoint MUST NOT act on a secret control packet that fails authentication.

### DC-0002-068 {#DC-0002-068}

An endpoint MUST find the path secret a received control packet names by the identifier of its own side of that path secret, derived from the identifier the packet carries.

### DC-0002-069 {#DC-0002-069}

An endpoint MUST request a handshake with the peer when it accepts an unknown path secret packet.

### DC-0002-070 {#DC-0002-070}

An endpoint MAY evict the path secret an accepted unknown path secret packet names, where its configuration asks for it.

### DC-0002-071 {#DC-0002-071}

An endpoint MUST NOT treat an unknown path secret packet as proof that its peer holds the path secret.

### DC-0002-072 {#DC-0002-072}

An endpoint SHOULD keep at most one secret control packet waiting to be sent for each path secret and sender it answers, and drop the others.
