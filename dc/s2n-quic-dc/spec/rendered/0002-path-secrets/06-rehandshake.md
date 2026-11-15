## Periodic rehandshake {#rehandshake}

An endpoint renews the path secret of each peer it holds one for by handshaking with the peer again within each rehandshake period. The cleaning pass of the previous section starts each minute's share of these handshakes.

### DC-0002-046 {#DC-0002-046}

An endpoint SHOULD handshake again with each peer it holds a path secret for about once per rehandshake period, 24 hours by default.

### DC-0002-047 {#DC-0002-047}

An endpoint SHOULD schedule periodic rehandshakes by going through its peers in turn, scheduling each peer once in each pass through them.

### DC-0002-048 {#DC-0002-048}

An endpoint SHOULD order the peers it handshakes with again by a hash keyed with a value it chooses at random.

### DC-0002-049 {#DC-0002-049}

An endpoint SHOULD spread the periodic rehandshakes of a period evenly over it, starting the share of them that each minute holds within that minute.

### DC-0002-050 {#DC-0002-050}

An endpoint SHOULD leave at least 100 milliseconds between the starts of two periodic rehandshakes.

### DC-0002-051 {#DC-0002-051}

An endpoint SHOULD run at most 2 periodic rehandshakes at once.
