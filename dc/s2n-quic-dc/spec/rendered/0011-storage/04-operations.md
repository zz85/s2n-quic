## Operations and their validation {#operations}

The kinds of operation are read, write, full flush, data flush and trim. A buffered operation uses a buffer the scheduler fills or reads from; a direct operation uses a caller-owned buffer aligned to the direct alignment of 4096 bytes, in place, without a copy. Each operation carries an opaque token its caller chooses, which its completion returns.

### DC-0011-019 {#DC-0011-019}

The device MUST keep the file of an operation open from its submission until its completion, even when the submitter closes its own handle to the file.

### DC-0011-020 {#DC-0011-020}

The device MUST reject, with an invalid-input error and before it acquires any credit, an operation whose offset plus length exceeds the largest file offset the platform can address.

### DC-0011-021 {#DC-0011-021}

The device MUST reject, with an invalid-input error and before it acquires any credit, a direct operation whose offset or length is not a multiple of the direct alignment of 4096 bytes.

### DC-0011-023 {#DC-0011-023}

A buffered read that reaches the end of the file MUST complete with the number of bytes it read, which can be fewer than its length.

### DC-0011-024 {#DC-0011-024}

A direct operation MUST transfer its data into or out of the caller's buffer in place, and return that buffer to the caller on completion.

### DC-0011-025 {#DC-0011-025}

The completion of an operation MUST carry the token its caller gave the operation.
