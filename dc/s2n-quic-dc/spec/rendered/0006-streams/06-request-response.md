## Request and response {#request-response}

A request-response exchange opens a stream, writes the whole request with the end-of-stream signal, and reads the response until its end. It writes and reads at the same time, and it returns once the response is complete.

### DC-0006-048 {#DC-0006-048}

A request-response exchange MUST write the whole request followed by the end-of-stream signal.

### DC-0006-053 {#DC-0006-053}

A request-response exchange MUST read the response until its end before it returns.

### DC-0006-049 {#DC-0006-049}

A request-response exchange MUST write the request and read the response at the same time.

### DC-0006-050 {#DC-0006-050}

A request-response exchange MUST fail with an error where the storage for the response can't take more bytes before the response ends.

### DC-0006-051 {#DC-0006-051}

A request-response exchange MUST fail as soon as one of its reads or writes fails.
