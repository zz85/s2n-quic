## Secret transfer message {#secret-transfer}

A secret transfer message moves a path secret between two processes on the same host. It holds a version byte, the ciphersuite, the exported secret, the application parameters with their own version byte, the time the sender encoded the message, and an opaque payload.

### DC-0004-071 {#DC-0004-071}

A secret transfer message MUST hold, in this order, a version byte, a ciphersuite byte, the export secret's length as a variable-length integer and its bytes, an application parameters version byte, the application parameters, an 8-byte encode time, and the payload's length as a variable-length integer and its bytes.

### DC-0004-072 {#DC-0004-072}

A secret transfer message MUST be sent only over a channel between processes on one host.

### DC-0004-073 {#DC-0004-073}

A receiver MUST NOT accept a secret transfer message whose version byte or application parameters version byte is other than 0.

### DC-0004-074 {#DC-0004-074}

A secret transfer message's ciphersuite byte MUST be 0 for AES-128-GCM with SHA-256 and 1 for AES-256-GCM with SHA-384.

### DC-0004-075 {#DC-0004-075}

A receiver MUST NOT accept a secret transfer message whose ciphersuite byte is other than 0 or 1.

### DC-0004-076 {#DC-0004-076}

The application parameters MUST hold, in this order, the maximum datagram size as a 2-byte integer, then the remote maximum data, the local send maximum data, the local receive maximum data, the maximum idle timeout in milliseconds with 0 for none, and the maximum number of queues, each a variable-length integer.

### DC-0004-077 {#DC-0004-077}

A receiver MUST NOT accept application parameters whose maximum idle timeout is larger than 4294967295 milliseconds.

### DC-0004-078 {#DC-0004-078}

The encode time of a secret transfer message MUST be its sender's raw monotonic clock in microseconds when it encoded the message.
