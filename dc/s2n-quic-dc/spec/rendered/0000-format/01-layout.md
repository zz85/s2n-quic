## Layout of the specification {#layout}

The specification lives in the spec directory of the crate. Its configuration, spec.toml, holds the identifier prefix, the levels, the implementation terms, the paths and the groups of RFCs. The rfcs directory holds the sources: the index of every RFC, and one directory for each written RFC, which holds the RFC's rfc.toml and one file for each section. The rendered directory and the .duvet/requirements directory hold the files the render command writes from those sources.

### DC-0000-001 {#DC-0000-001}

The specification MUST keep the sources of each RFC in a directory of its own under `rfcs`, named for the RFC's four-digit number and a short name.

### DC-0000-002 {#DC-0000-002}

Each RFC MUST have a row in `rfcs/index.toml` that holds its number, its title and its area.

### DC-0000-003 {#DC-0000-003}

The `rfc.toml` of each written RFC MUST hold the same number, title and area as the RFC's row in `rfcs/index.toml`.

### DC-0000-004 {#DC-0000-004}

Each section of an RFC MUST state the problem it addresses and the motivation for its requirements.

### DC-0000-005 {#DC-0000-005}

A person MUST NOT edit a file under `rendered` or `.duvet/requirements` by hand.
