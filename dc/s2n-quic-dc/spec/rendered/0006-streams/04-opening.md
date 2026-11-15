## Opening a stream {#opening}

A client opens a stream by naming the server's address and the acceptor identifier that the server routes the stream by. Where it holds a usable path secret for that server, the stream uses it; otherwise the client first runs a handshake. Opening doesn't send anything to the server: the stream is established with the server by its first write.

### DC-0006-029 {#DC-0006-029}

A client MUST open each stream toward a server address and an acceptor identifier.

### DC-0006-030 {#DC-0006-030}

Where the client doesn't hold a path secret for the server, opening a stream MUST run a handshake with the server before the stream is returned.

### DC-0006-031 {#DC-0006-031}

Opens of several streams toward one server that needs a handshake MUST share a single handshake.

### DC-0006-032 {#DC-0006-032}

While the client holds the server as unreachable and its cooldown hasn't passed, opening a stream toward that server MUST fail with a timeout.

### DC-0006-033 {#DC-0006-033}

Where the client's path secret for a server was marked unreachable, its cooldown has passed and the server has stayed silent past the idle timeout, opening a stream MUST discard that path secret and run a new handshake.

### DC-0006-034 {#DC-0006-034}

Until the server confirms a stream, each write of the client MUST carry the stream's acceptor identifier.

### DC-0006-035 {#DC-0006-035}

The client MUST report the server's rejection of a stream as an error of a later read or write on that stream.

### DC-0006-036 {#DC-0006-036}

Where the local resources a stream needs are closed, opening the stream MUST fail with an error.
