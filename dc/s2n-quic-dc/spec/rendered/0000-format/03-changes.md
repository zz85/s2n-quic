## Changing the specification {#changes}

A change to the specification is a pull request to the repository's main branch that edits the sources and holds the files the render command writes from them. A reviewer reads the change against this RFC, and once the review approves it, its writer merges it.

### DC-0000-015 {#DC-0000-015}

Each change to the specification MUST be proposed as a pull request to the repository's main branch.

### DC-0000-016 {#DC-0000-016}

A change to the specification MUST be reviewed by someone other than its writer before it is merged.

### DC-0000-017 {#DC-0000-017}

A change MUST hold the rendered documents and the requirement files of its sources, as the render command writes them, in the same pull request as the sources.

### DC-0000-018 {#DC-0000-018}

A change SHOULD pass the specification's check before it is merged.

### DC-0000-019 {#DC-0000-019}

The writer of a change MAY merge it once its review approves it.
