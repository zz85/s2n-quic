## Writing a requirement {#requirements}

Each requirement holds an identifier, a level, a polarity, a statement and a rationale. The statement holds one key word, which matches the level: MUST, SHOULD or MAY for a required obligation, and MUST NOT, SHOULD NOT or MAY NOT for a forbidden one. A requirement states what the system does, in terms a reader can observe, and leaves out how the code does it.

### DC-0000-006 {#DC-0000-006}

Each requirement MUST have an identifier of the form `DC-<rfc>-<n>`, where `<rfc>` is the four-digit number of its RFC and `<n>` is a number of at least three digits.

### DC-0000-007 {#DC-0000-007}

A change MUST NOT alter the identifier of a requirement that is on the main branch.

### DC-0000-008 {#DC-0000-008}

An RFC MUST list in its `retired` list the identifier of each requirement a change removes from it.

### DC-0000-009 {#DC-0000-009}

A retired identifier MUST NOT be reused for another requirement.

### DC-0000-010 {#DC-0000-010}

The statement of each requirement MUST hold exactly one key word, the one its level and its polarity set.

### DC-0000-011 {#DC-0000-011}

Each requirement MUST state one obligation.

### DC-0000-012 {#DC-0000-012}

Each requirement MUST state the reason for its obligation in its rationale.

### DC-0000-013 {#DC-0000-013}

A statement SHOULD describe what the system does as a peer or an operator observes it, rather than how its code is built.

### DC-0000-014 {#DC-0000-014}

A statement MUST NOT name a library, a programming language, a runtime or another choice of implementation.
