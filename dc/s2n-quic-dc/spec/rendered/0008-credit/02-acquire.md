## Acquiring and releasing credit {#acquire}

A holder asks for a number of bytes at a priority level. Where the pool's free credit covers the request and no request waits, the pool grants it at once. Otherwise the request waits at its level until the distribution grants it or the pool closes. A grant may be smaller than the request, and the holder then asks again for the rest. A holder releases each byte of credit it was granted once it is done with it.

### DC-0008-009 {#DC-0008-009}

A pool MUST lower each request above its level's largest single acquire to that amount.

### DC-0008-010 {#DC-0008-010}

Where the pool's free credit covers a request and no other request waits, the pool MUST grant the whole request at once, without the request waiting.

### DC-0008-011 {#DC-0008-011}

A pool MUST NOT grant a new request at once while another request waits for credit.

### DC-0008-012 {#DC-0008-012}

A request the pool can't grant at once MUST wait at its priority level behind the requests already waiting there.

### DC-0008-013 {#DC-0008-013}

A pool MUST answer each waiting request with either a grant of a number of bytes or the pool's close.

### DC-0008-014 {#DC-0008-014}

A wake of a waiting request that carries neither a grant nor the close MUST NOT end its wait.

### DC-0008-015 {#DC-0008-015}

A release MUST return its bytes to the pool.

### DC-0008-069 {#DC-0008-069}

A release MUST wake the distribution.
