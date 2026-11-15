## Writing a stream {#writing}

The write half splits what the application writes into frames that fit a packet, and sends them within the credit the peer granted and the local limit on data that is sent and not yet acknowledged. A write may queue only part of what the application offers. The application ends its direction by attaching the end-of-stream signal to its last write, or by shutting the write half down.

### DC-0006-016 {#DC-0006-016}

The write half MUST send bytes in the order the application writes them.

### DC-0006-017 {#DC-0006-017}

The write half MUST NOT send stream data past the limit the peer most recently granted.

### DC-0006-018 {#DC-0006-018}

The write half MUST report a write as successful once its bytes are queued for sending, whether or not the peer has received them.

### DC-0006-019 {#DC-0006-019}

The write half MUST let the application attach the end-of-stream signal to the last bytes it writes.

### DC-0006-020 {#DC-0006-020}

Shutting down the write half MUST close the stream's write direction and leave its read half open.

### DC-0006-021 {#DC-0006-021}

Shutting down the write half MUST succeed when the application repeats it.

### DC-0006-022 {#DC-0006-022}

After the write half has sent the end-of-stream signal or has been shut down, it MUST refuse further writes with an error.

### DC-0006-023 {#DC-0006-023}

The write half MUST let the application learn, without writing, that the peer has stopped reading, has reset the stream, or can't be reached.

### DC-0006-024 {#DC-0006-024}

Where a write that asked for the end of the stream, or a message the receiver assembles whole, is torn down before all of its bytes are sent, the write half MUST reset the stream instead of ending it cleanly.

### DC-0006-025 {#DC-0006-025}

When the application drops the write half without shutting it down, the write half SHOULD end the stream as a shutdown does.

### DC-0006-026 {#DC-0006-026}

Where the write half is dropped while the application is failing abnormally, it MUST reset the stream instead of ending it cleanly.

### DC-0006-027 {#DC-0006-027}

The write half MUST let the application send a whole message that the receiver assembles completely before it delivers any of it to the application.

### DC-0006-028 {#DC-0006-028}

The application MAY change the priority at which the write half asks for send credit.

### DC-0006-052 {#DC-0006-052}

A change of the write half's priority MUST apply only to the credit the write half asks for after the change.
