## Timing wheel {#timer-wheel}

A timing wheel holds pending entries, each with an optional target time, and releases each entry once its target has passed. It counts time in ticks of a fixed base length, which a component chooses for its use: a fine tick, such as a microsecond, for pacing and loss detection, and a coarse tick, such as a second, for idle timeouts. Entries arrive from a sender in order, and the wheel keeps that order among the entries that hold a target and are due at the same tick. An entry's target can be removed while the wheel holds it, which makes the entry due at once, but it can't move to a later time. The wheel keeps its entries in levels of growing span, and moves an entry to a finer level as its target comes near.

### DC-0012-019 {#DC-0012-019}

A timing wheel MUST hold an entry whose target lies fewer than 2^32 ticks after the wheel's current tick.

### DC-0012-020 {#DC-0012-020}

A timing wheel MUST release an entry that has no target time at once, without holding it.

### DC-0012-021 {#DC-0012-021}

A timing wheel MUST treat an entry whose target is at or before its current tick as due at the current tick.

### DC-0012-022 {#DC-0012-022}

When a timing wheel advances to a time, it MUST release every held entry whose target tick is at or before that time.

### DC-0012-023 {#DC-0012-023}

Entries from one sender that hold a target and are due at the same tick MUST be released in the order the sender sent them.

### DC-0012-024 {#DC-0012-024}

A timing wheel's current tick MUST NOT move backward, even where the clock reports an earlier time.

### DC-0012-025 {#DC-0012-025}

While a timing wheel holds an entry, it MUST keep its timer armed for a tick no later than the earliest held target.

### DC-0012-026 {#DC-0012-026}

A timing wheel that is empty MUST cancel its timer.

### DC-0012-027 {#DC-0012-027}

A sender MUST NOT move the target of an entry the wheel holds to a later time.

### DC-0012-028 {#DC-0012-028}

A component that receives an entry from a timing wheel MUST check the entry's current target, and either act on the entry or send it back to the wheel.

### DC-0012-029 {#DC-0012-029}

When a timing wheel's input closes, the wheel MUST release every entry it holds and every entry still waiting in its input.

### DC-0012-059 {#DC-0012-059}

A timing wheel MUST end once its input has closed and it has released every entry.
