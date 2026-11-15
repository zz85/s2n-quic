## Fast randomness {#randomness}

Load balancing and other choices that need speed rather than secrecy draw from a fast generator of random numbers. A component that balances load samples two candidates at random and takes the less loaded one.

### DC-0012-054 {#DC-0012-054}

The fast generator MUST NOT be used for a key, a nonce or any other value whose unpredictability the system's security depends on.

### DC-0012-055 {#DC-0012-055}

Outside a simulation, the fast generator MUST be seeded from the operating system's source of randomness.

### DC-0012-056 {#DC-0012-056}

Within a simulation, the fast generator MUST be seeded from the simulation's seed.

### DC-0012-057 {#DC-0012-057}

A uniform number drawn from the fast generator MUST be at least zero and less than one.

### DC-0012-058 {#DC-0012-058}

A component that balances load SHOULD sample two candidates at random and choose the less loaded one.
