## Event catalog {#catalog}

The endpoint reports events for its own life, for each datagram it sends and receives, for the path secret map that holds the secrets it shares with each peer, and for the keys of each stream. A measure on an event, such as a size, a count, a duration or a share of capacity, carries its unit.

### DC-0013-007 {#DC-0013-007}

The endpoint MUST report an event when it initializes, with each address it accepts connections on and handshakes on, and whether it serves datagram transport and stream transport.

### DC-0013-008 {#DC-0013-008}

The endpoint MUST report an event for each datagram it transmits and each datagram it receives, with the size of its buffer in bytes, its segment size in bytes and its segment count.

### DC-0013-009 {#DC-0013-009}

The endpoint MUST report an event for each transmission and each receive operation that fails, with the error it met.

### DC-0013-010 {#DC-0013-010}

The endpoint MUST report an event when the confirmation of a handshake or a path MTU probe times out, with the peer's address.

### DC-0013-011 {#DC-0013-011}

The path secret map MUST report an event for each entry it inserts, makes ready, replaces or evicts, with the age of each evicted entry.

### DC-0013-012 {#DC-0013-012}

The path secret map MUST report an event for each unknown path secret packet, replay detected packet and stale key packet that it sends, receives, accepts, rejects or drops.

### DC-0013-013 {#DC-0013-013}

The path secret map MUST report an event for each key it accepts, with the gap between that key and the highest key it accepted before, and how far its window of accepted keys moved forward.

### DC-0013-014 {#DC-0013-014}

The path secret map MUST report an event for each replay it detects, saying whether the replay is certain or only possible.

### DC-0013-015 {#DC-0013-015}

The path secret map MUST report an event for each lookup in its address cache and its identifier cache, saying whether the lookup found an entry and the age of the entry it found.

### DC-0013-016 {#DC-0013-016}

The path secret map MUST report an event for each cleaning cycle, with the share of its capacity in use and the time the cycle took.

### DC-0013-017 {#DC-0013-017}

A stream MUST report an event each time it updates its read key or its write key, with the new key phase.
