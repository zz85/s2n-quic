## Delivery {#delivery}

Tunneled datagrams are unreliable and unordered: a sender sends each one once, and it can arrive in any order or not at all. The payload is opaque: its meaning is the application's, and the sender and the receiver carry it as it is.

### DC-0009-025 {#DC-0009-025}

A sender MUST NOT send a tunneled datagram a second time after its first send.

### DC-0009-026 {#DC-0009-026}

A receiver MAY hand payloads to the application in another order than the order in which they were sent.

### DC-0009-027 {#DC-0009-027}

A receiver MUST hand the application each payload it accepts exactly as the sender's caller gave it.
