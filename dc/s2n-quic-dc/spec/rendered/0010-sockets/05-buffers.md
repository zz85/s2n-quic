## Packet buffers {#buffers}

Each datagram lives in a packet buffer that holds its payload, its remote address and its congestion marking. A coalesced read fills one buffer that is then split into one datagram for each segment, and the buffer is released once the last of them is. A released buffer goes back to the pool of the worker that allocated it, and a pool that runs out of buffers holds the reader back.

### DC-0010-030 {#DC-0010-030}

Each packet buffer MUST hold the payload, the remote address and the congestion marking of one datagram.

### DC-0010-031 {#DC-0010-031}

A receiver MUST NOT read from its socket without a free buffer to read into.

### DC-0010-032 {#DC-0010-032}

A buffer split into several datagrams MUST be released only once every one of those datagrams is released.

### DC-0010-033 {#DC-0010-033}

A released buffer SHOULD return to the pool of the worker that allocated it, to be reused instead of freed.

### DC-0010-034 {#DC-0010-034}

A released buffer whose pool has shut down MUST be freed.

### DC-0010-035 {#DC-0010-035}

A pool MAY allocate buffers before they are needed, so that its first allocations don't wait on the system allocator.

### DC-0010-036 {#DC-0010-036}

Where a socket coalesces received datagrams, its receive buffers SHOULD hold 65535 bytes.
