# Stdlib Crypto Library

`lib/crypto/` contains deterministic cryptographic primitives used as ordinary
stdlib libraries.

Keep this layer free of host entropy and OS policy. Randomness belongs behind
`sys.rand` or a caller-supplied source; this directory should stay pure and
testable from normal stdlib loads.
