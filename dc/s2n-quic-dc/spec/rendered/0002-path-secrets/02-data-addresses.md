## Peer data addresses {#data-addresses}

Each endpoint can advertise the addresses it receives data on in a transport parameter of the handshake. The parameter holds a version byte of 1, a count byte, and that many entries. Each entry holds an address type byte, a two-byte big-endian length and a value of that length. Address type 1 holds an 18-byte value: a 16-byte IPv6 address followed by a two-byte big-endian port. The receiving endpoint checks and normalizes the list against the address its handshake reached the peer at.

### DC-0002-013 {#DC-0002-013}

An endpoint MUST encode the data addresses it advertises in the format this section describes, each with address type 1.

### DC-0002-014 {#DC-0002-014}

An endpoint MUST write each IPv4 data address it advertises in its IPv4-mapped IPv6 form.

### DC-0002-015 {#DC-0002-015}

An endpoint MUST NOT fail a handshake for a data address entry whose address type or length it doesn't recognize, and it skips that entry instead.

### DC-0002-016 {#DC-0002-016}

An endpoint MUST treat a data address parameter that it can't decode, or whose version it doesn't support, as if the peer advertised an empty list.

### DC-0002-017 {#DC-0002-017}

An endpoint MUST fail a handshake with a transport parameter error where the peer advertises more than 128 data addresses.

### DC-0002-018 {#DC-0002-018}

An endpoint MUST fail a handshake with a transport parameter error where an advertised data address has port 0.

### DC-0002-019 {#DC-0002-019}

An endpoint MUST replace an unspecified IP address in an advertised data address with the IP address its handshake reached the peer at.

### DC-0002-020 {#DC-0002-020}

An endpoint MUST fail a handshake with a transport parameter error where a peer whose handshake address isn't a loopback address advertises a loopback data address.

### DC-0002-021 {#DC-0002-021}

An endpoint MUST map an advertised IPv4 data address to its IPv4-mapped IPv6 form where its handshake reached the peer at an IPv6 address.

### DC-0002-022 {#DC-0002-022}

An endpoint MUST fail a handshake with a transport parameter error where a peer whose handshake address is an IPv4 address advertises an IPv6 data address other than the unspecified address.

### DC-0002-023 {#DC-0002-023}

An endpoint MUST check the peer's data addresses before it obtains the path secret of the handshake.
