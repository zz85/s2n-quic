## Receiving {#receiving}

A receiver decodes a packet from the front of a datagram and leaves the bytes after it for the next packet. It rejects a packet whose first byte doesn't mark a datagram packet, marks routing information or an application header, or doesn't mark a packet number, and a packet whose wire version isn't zero. It opens the payload with its key, with the packet number as the nonce and the header as associated data, and hands the payload to the application only once the authentication tag verifies. The receiver's caller chooses that key, and a packet whose credentials identify another key fails the tag, since the tag authenticates the credentials.

### DC-0009-015 {#DC-0009-015}

A receiver MUST reject a packet whose first byte doesn't hold 0100 in its high four bits.

### DC-0009-016 {#DC-0009-016}

A receiver MUST reject a packet whose routing information bit is set.

### DC-0009-017 {#DC-0009-017}

A receiver MUST reject a packet whose application header bit is set.

### DC-0009-018 {#DC-0009-018}

A receiver MUST read the last 16 bytes of each packet as its authentication tag.

### DC-0009-019 {#DC-0009-019}

A receiver MUST NOT hand a payload to the application unless the packet's authentication tag verifies over its header and its payload, under the receiver's key and with its packet number as the nonce.

### DC-0009-020 {#DC-0009-020}

A datagram MAY carry more than one tunneled datagram packet, each starting at the byte after the end of the one before it.

### DC-0009-031 {#DC-0009-031}

A receiver MUST reject a packet whose wire version isn't zero.

### DC-0009-032 {#DC-0009-032}

A receiver MUST reject a packet whose packet number bit is clear.
