## Load and observability {#observability}

Each device reports its load as committed credit and the current capacity of each of its pools. It counts operations and bytes for each kind of operation, and counts failures, rejections and cancellations for the device as a whole.

### DC-0011-042 {#DC-0011-042}

The device MUST report its load as its pool capacity less its available credit, counting the credit that waiting submitters ask for, and never less than zero.

### DC-0011-043 {#DC-0011-043}

The device MUST count, for each kind of operation, the operations submitted and the bytes moved.

### DC-0011-044 {#DC-0011-044}

The device MUST count each operation that fails, each operation it rejects at admission and each reservation cancelled before submission.

### DC-0011-053 {#DC-0011-053}

The device MUST report the current capacity of each of its pools.
