## Queue slots {#queue-slots}

Each peer has a table of queue slots that routes each inbound stream frame and control frame to the handler of its stream. A stream's data can also arrive as a message in segments, and the receiver's wake point is the highest stream offset up to which a wake is owed: the end of the furthest stream data frame delivered, or of the furthest segment that ends the stream or asks for a wakeup. A slot is found by its index, the queue identifier both peers agree on when a stream opens. A slot carries a binding identifier, which says which stream holds the slot now, and two halves, one for stream frames and one for control frames. Each half records whether its sender is alive and whether a receiver holds it. The table grows by adding pages, each twice the size of the one before, and never moves a slot it has.

### DC-0012-036 {#DC-0012-036}

Dispatch MUST deliver a frame to a bound slot only where the frame's binding identifier equals the slot's current binding.

### DC-0012-062 {#DC-0012-062}

A server MUST accept the first frame of a stream for a free slot whose last binding is lower than the frame's binding, by binding the slot to the frame's stream.

### DC-0012-037 {#DC-0012-037}

Dispatch MUST reject a frame where its slot is free and the frame can't bind it, its binding is older or newer than the bound slot's, the half it targets has no receiver, or that half's sender is closed.

### DC-0012-061 {#DC-0012-061}

Dispatch MUST report the reason it rejected a frame.

### DC-0012-038 {#DC-0012-038}

A frame whose queue identifier is at or above the limit the peers agreed MUST be treated as a protocol violation.

### DC-0012-039 {#DC-0012-039}

A slot MUST NOT move while the table that holds it exists.

### DC-0012-040 {#DC-0012-040}

A half's sender, once closed, MUST NOT be opened again.

### DC-0012-041 {#DC-0012-041}

Delivering a stream data frame or a control frame to a half that has a receiver MUST wake the receiver's task.

### DC-0012-063 {#DC-0012-063}

Delivering a segment of a message MUST wake the receiver's task only where the segment ends the message, lies below the receiver's wake point, or carries the writer's blocked flag.
