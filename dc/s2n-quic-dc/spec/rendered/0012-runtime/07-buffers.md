## Byte buffers and allocation {#buffers}

Data the stack holds is kept in chunked byte buffers: an ordered list of byte chunks that are shared rather than copied. A builder fills a head buffer and turns each completed buffer into a chunk, and the policy it follows sets the head buffer's size and how a completed buffer becomes a chunk, such as a chunk that clears its memory once the last chunk that points into it is released. A tagged buffer counts the bytes it holds against an owner, so that an operator can read how many bytes each owner holds. Allocations are drawn from named sub-heaps.

### DC-0012-049 {#DC-0012-049}

A chunked buffer MUST yield its bytes in the order they were appended.

### DC-0012-050 {#DC-0012-050}

Appending a chunk to a chunked buffer MUST NOT copy the chunk's bytes.

### DC-0012-051 {#DC-0012-051}

A builder MUST turn each completed buffer into a chunk through its policy.

### DC-0012-052 {#DC-0012-052}

A tagged buffer MUST count against its owner the bytes it holds, for as long as it holds them.

### DC-0012-053 {#DC-0012-053}

A request to allocate zero bytes MUST NOT return an allocation.
