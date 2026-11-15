## Streams {#streams}

A stream carries two ordered byte sequences between a client and a server, one in each direction. Its read half receives what the peer writes, and its write half sends what the application writes. The application uses both halves through one stream, or separates them and uses each in its own task. Every stream is carried under a path secret that a handshake between the two hosts established, and it reports the identity and the data that handshake recorded.

### DC-0006-001 {#DC-0006-001}

A stream MUST carry two ordered byte sequences, one from the client to the server and one from the server to the client, through a read half and a write half.

### DC-0006-002 {#DC-0006-002}

A stream MUST let the application separate its read half from its write half and use each one independently of the other.

### DC-0006-003 {#DC-0006-003}

Each half of a stream MUST report as the peer's address the address that the handshake of its path secret used, even where data travels over other paths.

### DC-0006-004 {#DC-0006-004}

Each half of a stream MUST give the application the data that the handshake of its path secret recorded for that peer.

### DC-0006-005 {#DC-0006-005}

A reset of a stream MUST end both of its halves at once.

### DC-0006-006 {#DC-0006-006}

A stream that is reset SHOULD tell the peer about the reset.

### DC-0006-007 {#DC-0006-007}

A stream SHOULD yield to other tasks after it has handled a bounded number of frames in one turn, and resume in a later turn.
