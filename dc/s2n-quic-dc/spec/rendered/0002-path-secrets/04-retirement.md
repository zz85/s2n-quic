## Retiring and evicting path secrets {#retirement}

An endpoint runs a cleaning pass over its secret map at a steady interval. Each pass evicts the retired path secrets whose grace period has ended, and it also starts the periodic rehandshakes of the next section.

### DC-0002-033 {#DC-0002-033}

An endpoint SHOULD run a cleaning pass over its secret map about once a minute.

### DC-0002-034 {#DC-0002-034}

An endpoint SHOULD start each cleaning pass at a random time within its minute.

### DC-0002-035 {#DC-0002-035}

An endpoint SHOULD evict a retired path secret once 10 cleaning passes have run since it was retired.
