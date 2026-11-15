## Requesting handshakes {#handshake-requests}

An endpoint asks for a handshake with a peer when it has no path secret for a flow to the peer, when the periodic rehandshake reaches the peer, when it accepts an unknown path secret or replay detected packet from the peer, and when a path secret is dead and silent. A path secret is dead when flows opened under it go unanswered, and silent when the peer hasn't sent an authenticated packet under it for one idle timeout.

### DC-0002-036 {#DC-0002-036}

An endpoint MUST join each request for a handshake with a peer address to the handshake with that address that is already running, where one runs.

### DC-0002-037 {#DC-0002-037}

An endpoint SHOULD bound the number of handshakes it starts concurrently, to 5.

### DC-0002-038 {#DC-0002-038}

An endpoint SHOULD bound the number of handshake connections it holds open at once, to 750.

### DC-0002-039 {#DC-0002-039}

After a successful handshake with a peer address, an endpoint SHOULD answer the requests for a handshake with that address with that success, without handshaking, for a random period of up to 1 minute.

### DC-0002-040 {#DC-0002-040}

After a failed handshake with a peer address, an endpoint SHOULD answer the requests for a handshake with that address with that failure, without handshaking, for a random period between 1 second and 2 minutes.

### DC-0002-041 {#DC-0002-041}

An endpoint MUST mark a path secret dead when a flow it opens under that path secret goes unanswered by the peer.

### DC-0002-042 {#DC-0002-042}

An endpoint MUST clear the dead mark of a path secret when it receives an authenticated packet under that path secret.

### DC-0002-043 {#DC-0002-043}

An endpoint SHOULD refuse new flows under a path secret marked dead until the dead-peer cooldown since the mark has passed.

### DC-0002-044 {#DC-0002-044}

An endpoint SHOULD evict a path secret that is marked dead, whose dead-peer cooldown has passed, and that is silent.

### DC-0002-073 {#DC-0002-073}

An endpoint SHOULD handshake again with the peer of a path secret it evicts for being dead and silent.

### DC-0002-045 {#DC-0002-045}

An endpoint MUST take a path secret's idle timeout as the maximum idle timeout the peer advertised in the handshake, or as 60 seconds where the peer advertised none.
