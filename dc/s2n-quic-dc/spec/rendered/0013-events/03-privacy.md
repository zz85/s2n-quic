## Credentials and secrets in diagnostics {#privacy}

An event delivered to the subscriber may carry the identifier of the credential a connection uses, so that the application can group the events of one peer. The snapshot form of an event, taken for a test, hides that identifier, while the ordinary debug form of an event and the integration that writes events as log lines both write it, so a debug-level log of events holds it. No event, metric or log line carries key material.

### DC-0013-018 {#DC-0013-018}

The snapshot form of an event MUST NOT show the value of a credential identifier.

### DC-0013-019 {#DC-0013-019}

An event, a metric or a log line MUST NOT carry the bytes of a key or of a path secret.

### DC-0013-020 {#DC-0013-020}

An event delivered to the subscriber MAY carry the identifier of the credential a connection uses.
