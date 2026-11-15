## Versions {#versions}

A dc version is a whole number. Each implementation holds the versions it supports in order of preference. The client lists its versions in a transport parameter of the handshake, the server selects one and answers with it in the same parameter, and both peers then speak the version the server selected. Where the server selects none, the connection doesn't use the dc protocol. Version 0 is the only version this specification defines.

### DC-0001-021 {#DC-0001-021}

A dc version MUST be a whole number from 0 to 4294967295.

### DC-0001-022 {#DC-0001-022}

A client MUST list the dc versions it supports in the transport parameter 0xdc0000.

### DC-0001-023 {#DC-0001-023}

A client MUST list its dc versions in its order of preference, the most preferred first.

### DC-0001-024 {#DC-0001-024}

A client MUST NOT list more than four dc versions.

### DC-0001-025 {#DC-0001-025}

A peer MUST encode each dc version in the transport parameter 0xdc0000 as a variable-length integer.

### DC-0001-026 {#DC-0001-026}

A server MUST select the first of its own supported dc versions, in its order of preference, that the client's list holds.

### DC-0001-027 {#DC-0001-027}

A server that selects a dc version MUST send that version alone in the transport parameter 0xdc0000.

### DC-0001-028 {#DC-0001-028}

A server MUST ignore each version of a client's list after its fourth.

### DC-0001-029 {#DC-0001-029}

A peer MUST NOT use the dc protocol on a connection where the server didn't select a dc version.

### DC-0001-030 {#DC-0001-030}

An endpoint that doesn't run the dc protocol MUST close the connection with an error when it receives a dc-specific frame.

### DC-0001-031 {#DC-0001-031}

Every implementation MUST support dc version 0.

### DC-0001-032 {#DC-0001-032}

An endpoint that runs the dc protocol MUST ignore each dc-specific frame it receives on a connection where it doesn't use the dc protocol.
