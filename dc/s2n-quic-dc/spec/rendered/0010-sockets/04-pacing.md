## Pacing {#pacing}

This section states the socket's send-rate pacer; RFC 0007 states the pacing that congestion control sets. A paced sender keeps a credit, measured in time, that grows with elapsed time at its configured rate up to a burst of one largest send. Each item it sends takes its cost out of the credit. An item may take the credit below zero, and the sender then waits until the credit is restored. Credit is charged only for items that are sent.

### DC-0010-024 {#DC-0010-024}

A paced sender MUST hold its average sending rate to the rate it is configured with, in bits per second.

### DC-0010-025 {#DC-0010-025}

A paced sender's burst MUST NOT exceed the time its configured rate takes to send 65535 bytes.

### DC-0010-026 {#DC-0010-026}

A paced sender's credit MUST grow with elapsed time at its configured rate, up to its burst.

### DC-0010-027 {#DC-0010-027}

A paced sender MAY send an item that takes its credit below zero.

### DC-0010-028 {#DC-0010-028}

A paced sender whose credit is below zero MUST wait until elapsed time restores its credit before it sends the next item.

### DC-0010-029 {#DC-0010-029}

An item that is cancelled or skipped before it is sent MUST NOT consume a paced sender's credit.
