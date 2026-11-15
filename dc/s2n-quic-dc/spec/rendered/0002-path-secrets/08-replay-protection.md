## Replay protection {#replay-protection}

A receiver tracks, for each path secret, the highest key identifier it has seen and which key identifiers it has seen in a window of 896 identifiers that ends at that highest one. A stale key packet tells the sender to move its next key identifier past the highest one the receiver has seen.

### DC-0002-058 {#DC-0002-058}

An endpoint MUST record a received key identifier as seen only after the packet that carries it is authenticated.

### DC-0002-059 {#DC-0002-059}

An endpoint MUST track, for each path secret, which key identifiers it has seen in a window of 896 key identifiers that ends at the highest one it has seen.

### DC-0002-060 {#DC-0002-060}

An endpoint MUST reject a packet as a replay where it has already recorded the packet's key identifier as seen under that path secret.

### DC-0002-061 {#DC-0002-061}

An endpoint MUST reject a packet as a possible replay where the packet's key identifier is below the window it tracks.

### DC-0002-062 {#DC-0002-062}

An endpoint SHOULD answer a packet it rejects as a replay with a replay detected packet that carries the rejected key identifier.

### DC-0002-063 {#DC-0002-063}

An endpoint SHOULD answer a packet it rejects as a possible replay with a stale key packet that carries one more than the highest key identifier it has seen, or 0 where it has seen none.

### DC-0002-064 {#DC-0002-064}

An endpoint MUST NOT deliver the payload of a packet it rejects as a replay or a possible replay.
