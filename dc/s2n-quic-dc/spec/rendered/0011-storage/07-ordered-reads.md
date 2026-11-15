## Ordered reads across devices {#ordered-reads}

An ordered read takes a sequence of blocks. A block is either a range of a file on a device, with a number of bytes to trim from its head and from its tail, or bytes already in memory. The ordered read reads the device blocks, buffered or direct, and delivers each block's bytes in the order of the sequence. Its window holds every block it has taken from the sequence and not yet delivered: blocks waiting for credit, blocks being read, and blocks read and waiting for their turn.

### DC-0011-036 {#DC-0011-036}

An ordered read MUST deliver the blocks' bytes in the order of the block sequence, whatever order their reads complete in.

### DC-0011-037 {#DC-0011-037}

An ordered read MUST NOT let a block waiting for credit on one device delay the submission of a block on another device.

### DC-0011-038 {#DC-0011-038}

An ordered read MUST deliver a block of in-memory bytes in its place in the order, without reading a device or acquiring credit.

### DC-0011-039 {#DC-0011-039}

An ordered read MUST remove a device block's head trim and tail trim from the bytes it delivers for that block.

### DC-0011-040 {#DC-0011-040}

An ordered read MUST deliver a block's validation error in that block's place in the order.

### DC-0011-041 {#DC-0011-041}

An ordered read MUST bound the number of blocks it has taken from the sequence and not yet delivered.
