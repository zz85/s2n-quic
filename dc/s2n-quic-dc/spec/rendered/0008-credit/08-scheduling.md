## Scheduling work from several sources {#scheduling}

A scheduler pulls items from a pipeline of sources. Each turn has a budget of items. A priority merge of several sources starts each turn at its highest source and polls them in turn. A priority select serves a priority source ahead of a fallback source and tells its consumer whether more priority items wait, so that the consumer can hold back work of low urgency. A paced source spaces its items by their cost at a rate, with a token bucket that allows a short burst after an idle time and can go into debt.

### DC-0008-056 {#DC-0008-056}

The credit pool and the rules of this section MUST count cost in abstract units, without reference to a network protocol.

### DC-0008-057 {#DC-0008-057}

A scheduler MUST process at most its budget of items in one turn.

### DC-0008-058 {#DC-0008-058}

Where a turn ends on its budget with items left, the scheduler MUST run again without waiting for a new item.

### DC-0008-059 {#DC-0008-059}

A priority merge MUST start each turn at its highest-priority source.

### DC-0008-060 {#DC-0008-060}

Within a turn, a priority merge MUST poll its sources in turn, starting after the source it last served.

### DC-0008-061 {#DC-0008-061}

A priority merge MUST close once each of its sources is closed.

### DC-0008-062 {#DC-0008-062}

A priority select MUST take an item from its fallback source only while its priority source has none.

### DC-0008-063 {#DC-0008-063}

A priority select MUST close when its priority source closes.

### DC-0008-064 {#DC-0008-064}

With each item, a priority select MUST tell its consumer whether its priority source holds another item, or that its budget ran out before it could look.

### DC-0008-065 {#DC-0008-065}

A paced source MUST charge an item's cost to its token bucket only once its consumer confirms it processed the item.

### DC-0008-066 {#DC-0008-066}

A paced source MUST hold its next item while its token bucket is in debt, until the rate repays the debt.

### DC-0008-067 {#DC-0008-067}

A token bucket MUST refill at its rate over the time since its last use, up to its burst.

### DC-0008-068 {#DC-0008-068}

A token bucket's burst SHOULD be 65535 bytes of cost at its rate.

### DC-0008-083 {#DC-0008-083}

A priority select MUST close when its fallback source closes while its priority source holds no item.
