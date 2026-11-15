## Pipeline channels {#pipeline}

Received and sent items pass between stages through channels. A pipeline that is given a budget of items for each poll cycle processes items until its budget runs out and then yields, and wakes itself once where items remain. A two-input priority merge serves its priority input first, and a priority merge over a list of inputs starts each poll cycle at its highest-priority input and then takes the inputs in turn. A stage that spreads items over several outputs takes the outputs in turn, and moves every item its input holds in one poll.

### DC-0010-037 {#DC-0010-037}

Each poll of a pipeline stage that is given a budget for its poll cycle MUST process at most the items that budget allows and then yield.

### DC-0010-038 {#DC-0010-038}

A pipeline whose poll ends with items left MUST wake itself once to continue.

### DC-0010-039 {#DC-0010-039}

A two-input priority merge MUST take an item from its priority input whenever one is ready, before it takes one from its other input.

### DC-0010-051 {#DC-0010-051}

A priority merge over a list of inputs MUST start each poll cycle at its highest-priority input, and start each later take in that cycle at the input after the one it took last.

### DC-0010-040 {#DC-0010-040}

An item a priority merge reads ahead MUST be delivered by its next receive.

### DC-0010-041 {#DC-0010-041}

A socket sender stage that fails to send an item MUST return the item with the error, so that the caller releases it.

### DC-0010-042 {#DC-0010-042}

A stage that spreads items across several outputs SHOULD give each item to the next output in turn after the one it used last.

### DC-0010-052 {#DC-0010-052}

A stage that spreads items across several outputs MAY move every item its input holds in one poll, until its input or every output is pending.
