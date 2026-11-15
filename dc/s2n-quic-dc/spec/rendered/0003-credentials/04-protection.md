## Packet protection and authentication {#protection}

Application data is sealed with the ciphersuite's AEAD algorithm. Control packets carry no secret payload and are authenticated with HMAC instead. A retransmitted control packet keeps its original tag, adjusted by a value derived from both of its packet numbers.

### DC-0003-026 {#DC-0003-026}

The AEAD nonce of a packet MUST be its key's 12-byte IV with the last 8 bytes combined by exclusive-or with the packet number as an 8-byte big-endian integer.

### DC-0003-027 {#DC-0003-027}

The AEAD associated data of a packet MUST be the packet's header.

### DC-0003-028 {#DC-0003-028}

The AEAD tag of a packet MUST be 16 bytes.

### DC-0003-029 {#DC-0003-029}

A sender MUST NOT seal two packets with the same key and the same packet number.

### DC-0003-030 {#DC-0003-030}

The tag of a control packet MUST be the HMAC of its header under the sender's control key, truncated to the tag length of its packet type.

### DC-0003-031 {#DC-0003-031}

The tag length of a stream control packet MUST be 16 bytes.

### DC-0003-032 {#DC-0003-032}

A receiver MUST compare a control tag with the tag it computes in time that doesn't depend on the bytes in which they differ.

### DC-0003-033 {#DC-0003-033}

A receiver MUST refuse a control tag whose length differs from the tag length of its packet type.

### DC-0003-034 {#DC-0003-034}

The tag of a retransmitted stream control packet MUST be its original tag combined by exclusive-or with the leading bytes of the HMAC, under the same control key, of the original packet number followed by the retransmission's packet number, each as an 8-byte big-endian integer.

### DC-0003-035 {#DC-0003-035}

An endpoint MAY run a stream without control keys over a transport that delivers packets reliably and in order.

### DC-0003-049 {#DC-0003-049}

An endpoint that runs a stream without control keys MUST refuse each control-tag operation on that stream, verifying a control tag and making a retransmission tag alike.
