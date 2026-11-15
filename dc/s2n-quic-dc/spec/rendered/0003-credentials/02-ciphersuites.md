## Ciphersuites {#ciphersuites}

A ciphersuite sets the AEAD algorithm that protects application data, the hash function of the key derivation and the hash function of the message authentication code that protects control packets. Each path secret is bound to one ciphersuite and holds a 32-byte export secret, from which every key of the path is derived.

### DC-0003-007 {#DC-0003-007}

Ciphersuite 0 MUST use AES-128-GCM for packet protection, HKDF with SHA-256 for key derivation and HMAC with SHA-256 for control authentication.

### DC-0003-008 {#DC-0003-008}

Ciphersuite 1 MUST use AES-256-GCM for packet protection, HKDF with SHA-384 for key derivation and HMAC with SHA-384 for control authentication.

### DC-0003-009 {#DC-0003-009}

An endpoint MUST refuse a ciphersuite value other than 0 and 1.

### DC-0003-010 {#DC-0003-010}

The key length of a ciphersuite MUST be 16 bytes for ciphersuite 0 and 32 bytes for ciphersuite 1.

### DC-0003-011 {#DC-0003-011}

Each path secret MUST hold an export secret of 32 bytes and the one ciphersuite it was established with.
