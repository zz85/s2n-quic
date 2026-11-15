## Clocks and timers {#clock}

A clock reports the current time and creates timers. Time is a count of nanoseconds since the clock's start. A timer has an optional target, and while it has one it is armed: a task waiting on it wakes once the clock reaches the target. A timer's target can move, and the timer can be cancelled. The inspector executor runs nothing, so its clock doesn't advance and its timers never fire.

### DC-0012-011 {#DC-0012-011}

A clock MUST report time as a count of nanoseconds since its start that never decreases.

### DC-0012-012 {#DC-0012-012}

Adding a duration to a time, or subtracting one from it, MUST saturate at the least and the greatest time instead of wrapping.

### DC-0012-013 {#DC-0012-013}

The duration from a time to an earlier time MUST be reported as zero.

### DC-0012-014 {#DC-0012-014}

A task waiting on an armed timer MUST be woken once the clock reaches the timer's target.

### DC-0012-015 {#DC-0012-015}

A timer MUST NOT report that it is ready before the clock reaches its target.

### DC-0012-016 {#DC-0012-016}

A timer whose target is updated MUST wake its task at the new target instead of the old one.

### DC-0012-017 {#DC-0012-017}

A cancelled timer MUST report that it isn't armed.

### DC-0012-018 {#DC-0012-018}

A time MUST be displayed as its whole seconds, a dot, and nine digits of nanoseconds.
