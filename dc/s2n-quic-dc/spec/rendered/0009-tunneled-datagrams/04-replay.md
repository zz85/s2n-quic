## Replay protection {#replay}

A receiver keeps one sliding window of the packet numbers it has accepted from a sender, across every key phase it can still open. Once a packet's tag verifies, the receiver checks its number against the window and records it there in one step. A number in the window that the receiver accepted before is a repeat, and a number older than the window's oldest is too old: the receiver rejects both. A number newer than the window's newest moves the window on.

### DC-0009-021 {#DC-0009-021}

A receiver MUST, once a packet's authentication tag verifies, check its packet number against its replay window and record it there in one step, and reject the packet where that step finds the number accepted before.

### DC-0009-022 {#DC-0009-022}

A receiver MUST reject a packet whose packet number is older than the oldest number its replay window holds.

### DC-0009-023 {#DC-0009-023}

A receiver MUST accept, in its replay check, a packet whose packet number is newer than every number it accepted before.

### DC-0009-024 {#DC-0009-024}

A receiver MUST keep one replay window for each sender's packet numbers, holding the numbers accepted under every key phase it can still open.
