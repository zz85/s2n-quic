## Recycling queue identifiers {#queue-recycling}

When a server drops its receivers for a stream, it tells the client that the stream's queue identifier is free, so that the client can use it for another stream. The server gathers the identifiers freed until a free message is sent and sends them together. The client allocates fresh identifiers up to the agreed limit, and then reuses the freed ones. Each new free message carries a request number that increases, and a message sent again keeps its number, so that the client can find a message it has already applied.

### DC-0012-042 {#DC-0012-042}

A server MUST tell the client each queue identifier whose receivers it has dropped.

### DC-0012-043 {#DC-0012-043}

A server SHOULD send in one free message every identifier freed since its last free message was sent.

### DC-0012-044 {#DC-0012-044}

Each new free message MUST carry a request number greater than that of every free message the server sent to the same client before it.

### DC-0012-064 {#DC-0012-064}

A free message that the server sends again MUST keep its request number.

### DC-0012-045 {#DC-0012-045}

A client MUST ignore a free message whose request number it has already applied.

### DC-0012-046 {#DC-0012-046}

A client MUST allocate a fresh identifier below the agreed limit, while one remains, before it reuses a freed one.

### DC-0012-047 {#DC-0012-047}

A client waiting for a queue identifier MUST be woken once an identifier is freed or the peer's list of identifiers is closed.

### DC-0012-048 {#DC-0012-048}

A client MUST NOT allocate a fresh queue identifier at or above the limit the peers agreed.
