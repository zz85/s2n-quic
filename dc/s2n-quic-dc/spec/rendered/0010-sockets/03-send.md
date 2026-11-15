## Sending datagrams {#send}

A send hands the network stack one or more segments for one destination, with one congestion marking. Where it holds more than one segment, the stack splits it into datagrams of the segments' size: every segment but the last is that size, the last isn't longer, and the segments of one send total at most 65535 bytes.

### DC-0010-016 {#DC-0010-016}

Each send MUST address all of its segments to one destination and give them one congestion marking.

### DC-0010-017 {#DC-0010-017}

Every segment of a send except the last MUST be the same size.

### DC-0010-018 {#DC-0010-018}

The last segment of a send MUST NOT be longer than the segments before it.

### DC-0010-019 {#DC-0010-019}

The segments of one send MUST NOT total more than 65535 bytes.

### DC-0010-020 {#DC-0010-020}

A send of more than one segment MUST ask the network stack to split it into datagrams of the segments' size.

### DC-0010-021 {#DC-0010-021}

A send whose congestion marking is anything other than not congestion capable MUST set that marking on each datagram it sends.

### DC-0010-022 {#DC-0010-022}

A send of a single segment MAY go without a segment size.

### DC-0010-023 {#DC-0010-023}

A sender SHOULD read its socket's local address once and keep it, instead of asking the operating system for it on each use.
