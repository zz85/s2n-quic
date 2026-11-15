## Rejected keys and silent peers {#peers}

A peer tells the endpoint that it doesn't know a path secret, that a key is stale, or that it detected a replay, through the notices RFC 0002 defines. The endpoint acts on such a notice only once it has validated it against its path secrets. A send state that receives nothing from its peer for the idle timeout is discarded; where it still held packets the peer never acknowledged, the peer is declared dead, unless the cooldown of an earlier declaration is still running.

### DC-0005-040 {#DC-0005-040}

The endpoint MUST NOT change any send or receive state on a notice from a peer that fails validation against its path secrets.

### DC-0005-041 {#DC-0005-041}

On a validated unknown path secret notice, the endpoint MUST fail every pending and unacknowledged frame sent under that credential with the unknown path secret reason.

### DC-0005-042 {#DC-0005-042}

On a validated unknown path secret notice, the endpoint MUST discard its receive state for that credential.

### DC-0005-043 {#DC-0005-043}

On a validated stale key or replay notice, the sender MUST queue for retransmission the pending and unacknowledged frames of the send state that used the rejected key.

### DC-0005-044 {#DC-0005-044}

A sender MUST NOT discard or retransmit the frames of a send state whose key is newer than the key a stale key or replay notice rejects.

### DC-0005-045 {#DC-0005-045}

A sender MUST discard a send state once neither an acknowledgement nor any other packet has arrived from its peer for the idle timeout.

### DC-0005-046 {#DC-0005-046}

When a sender discards a send state for idleness, it MUST fail each of its pending and unacknowledged frames with the peer dead reason.

### DC-0005-047 {#DC-0005-047}

When a send state discarded for idleness still held packets its peer never acknowledged, and the cooldown of an earlier declaration of the peer as dead isn't running, the endpoint MUST reset every stream with that peer.

### DC-0005-048 {#DC-0005-048}

When the peer of a client path is declared dead, the client MUST request a new handshake with that peer.

### DC-0005-049 {#DC-0005-049}

A receiver MUST NOT declare a peer dead because its receive state went idle.

### DC-0005-050 {#DC-0005-050}

The endpoint MUST NOT open a new stream to a peer during the cooldown that follows the peer being declared dead.
