## Devices and the registry {#devices}

A device is the limited resource that storage operations share, together with the scheduler that admits and runs them. A registry owns the execution backend and creates devices; each device it creates holds its own admission budget, cost model and execution lanes. A device's label names it in diagnostics alone.

### DC-0011-001 {#DC-0011-001}

Each device MUST admit, order and run its operations by itself, without a scheduler shared with other devices.

### DC-0011-002 {#DC-0011-002}

The registry MUST accept the registration of a new device from any thread at any time after the registry is created.

### DC-0011-003 {#DC-0011-003}

A registration MUST fail with a broken-pipe error once the registry has been torn down.

### DC-0011-004 {#DC-0011-004}

The scheduler MUST NOT use a device's label for anything other than naming the device in diagnostics and metrics.
