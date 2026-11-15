## Cancelling a wait and closing the pool {#close}

A holder can cancel its wait at any time. Where the cancel and a grant race, one of them takes effect: the holder keeps the grant and releases it later, or the pool keeps the credit. The distribution reclaims each cancelled wait it meets, and a cancelled wait at the head of a level doesn't hold back the waiters behind it. When the distribution stops, the pool closes: it answers each waiting request with the close, answers each later request with the close, and grants no more credit. A release to a closed pool succeeds and has no effect.

### DC-0008-038 {#DC-0008-038}

A holder MUST be able to cancel its waiting request.

### DC-0008-039 {#DC-0008-039}

Where a cancel and a grant of the same request race, the pool MUST let exactly one of them take effect.

### DC-0008-040 {#DC-0008-040}

A pool MUST return the request of a cancelled wait to its free credit.

### DC-0008-041 {#DC-0008-041}

A cancelled wait at the head of a level MUST NOT hold back the waiters behind it.

### DC-0008-042 {#DC-0008-042}

When its distribution stops, a pool MUST answer each waiting request with the close.

### DC-0008-043 {#DC-0008-043}

A pool MUST answer a request made after it closed with the close, without the request waiting.

### DC-0008-044 {#DC-0008-044}

A closed pool MUST NOT grant credit.

### DC-0008-045 {#DC-0008-045}

A release to a closed pool MUST NOT fail.
