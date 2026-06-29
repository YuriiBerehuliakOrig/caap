# Stdlib crypto safety policy

The `stdlib.lib.crypto.*` modules are **pure, deterministic** CAAP — no clock, no
randomness, no I/O — implementing common digests, MACs, key-derivation functions,
and non-cryptographic checksums. This page states which APIs are safe defaults,
which are legacy-only, and the guarantees they do and do **not** make. Every claim
is grounded in a source location (`file:symbol`).

The modules:

| Module | Source | Provides |
|---|---|---|
| `stdlib.lib.crypto.digest` | [digest.caap](../../stdlib/lib/crypto/digest.caap) | `sha256`, `sha1`, `md5`, generic `hmac` |
| `stdlib.lib.crypto.legacy` | [legacy.caap](../../stdlib/lib/crypto/legacy.caap) | `sha1`, `md5` re-exported (the conspicuous "weak digest" import path) |
| `stdlib.lib.crypto.kdf` | [kdf.caap](../../stdlib/lib/crypto/kdf.caap) | `pbkdf2_sha256`, `hkdf_extract`, `hkdf_expand`, `hkdf_sha256` |
| `stdlib.lib.crypto.checksum` | [checksum.caap](../../stdlib/lib/crypto/checksum.caap) | `crc32c`, `adler32`, `checksum` dispatcher (+ re-exported `crc32`, `fnv1a`, `fnv1a_64`) |

## Safe defaults vs legacy

**Use for new security work** (collision-resistant / standard constructions):

- `sha256` ([digest.caap:sha256](../../stdlib/lib/crypto/digest.caap)) — the
  default cryptographic digest (content addressing, subresource integrity).
- `hmac` over `sha256` — `(hmac sha256 key msg)`
  ([digest.caap:hmac](../../stdlib/lib/crypto/digest.caap)). The generic `hmac`
  takes the hash *function* as its first argument; pin it to `sha256`.
- `pbkdf2_sha256` and the HKDF family
  ([kdf.caap](../../stdlib/lib/crypto/kdf.caap)) — both instantiated over
  HMAC-SHA-256, the safe PRF.

**Legacy / interop ONLY — never for new security work:**

- `sha1`, `md5` ([digest.caap](../../stdlib/lib/crypto/digest.caap), re-exported
  through [legacy.caap](../../stdlib/lib/crypto/legacy.caap)). Both are
  cryptographically **broken**: practical collisions exist (MD5 since 2004,
  SHA-1's *SHAttered* collision in 2017), so neither offers collision resistance.
  They remain only for interop with formats/protocols that still mandate them.
  Importing them from `stdlib.lib.crypto.legacy` (rather than `digest`) is the
  in-code signal that a weak digest is a conscious legacy choice, not an oversight.
- `(hmac md5 …)` / `(hmac sha1 …)` — HMAC repairs the *MAC* construction even
  over a weak hash, so it is acceptable for interop with HMAC-MD5 / HMAC-SHA1
  systems (those vectors are pinned in
  [test_digest.caap](../../stdlib/lib/tests/test_digest.caap)) — but prefer
  HMAC-SHA-256 for anything new.

SHA-512 is **deferred**: it needs 64-bit-lane arithmetic the kernel i64 cannot
hold before masking; see the note in
[digest.caap](../../stdlib/lib/crypto/digest.caap).

## What these APIs do NOT guarantee

- **Not constant-time; no side-channel resistance.** These are straightforward
  pure-CAAP ports running on a tree-walking evaluator. They make **no** timing,
  cache, or branch-predictor guarantees. Do **not** compare a computed MAC/digest
  against a secret with an early-exit equality, and do not treat any of these as
  hardened against a side-channel adversary. For that, use a vetted native
  implementation behind a `sys.*` facade.
- **No randomness / nonces / salts are generated here.** Everything is
  deterministic by design ([kdf.caap](../../stdlib/lib/crypto/kdf.caap) header);
  the caller supplies salts/IKM. A KDF is only as strong as a caller-supplied
  high-entropy salt.

## Checksum vs cryptographic hash

[checksum.caap](../../stdlib/lib/crypto/checksum.caap) provides **error-detecting
codes** (CRC-32, CRC-32C, Adler-32, FNV-1a) — used by archive/binary formats and
transport layers to catch accidental corruption. They are **not** message
digests and offer **no** security: an adversary can trivially forge an input with
a target checksum. The three categories live in three modules and must not be
confused:

- `crypto.checksum` — integrity against *accidental* corruption (CRC/Adler/FNV).
- `crypto.digest` — cryptographic message digests (SHA-256; SHA-1/MD5 legacy).
- `lib.hash` — in-memory dictionary hashing (CRC-32/FNV-1a are re-exported from
  there into `checksum` so there is one implementation).

The `checksum` dispatcher rejects an unknown algorithm name with a **structured
diagnostic error** rather than a silent fallback: it `raise!`s a
`stdlib.lib.diag.error` protocol error
([error.caap:raise!](../../stdlib/lib/diag/error.caap)) with code
`stdlib.crypto.checksum.unknown_algorithm` and the offending name under data
`"name"`. A catcher dispatches on the code; the change from the earlier bare
thrown string is observable to any caller that inspected the caught value.

## Parameter-validation contract

The KDFs **enforce** (not merely document) their parameter bounds, raising a
`stdlib.lib.diag.error` protocol error on a violation
([kdf.caap](../../stdlib/lib/crypto/kdf.caap)):

| Function | Constraint | Error code |
|---|---|---|
| `pbkdf2_sha256` | `iterations >= 1` | `stdlib.crypto.kdf.invalid_iterations` |
| `pbkdf2_sha256` | `dklen >= 0` | `stdlib.crypto.kdf.invalid_dklen` |
| `hkdf_expand` | `length >= 0` | `stdlib.crypto.kdf.invalid_hkdf_length` |
| `hkdf_expand` | `length <= 8160` (255*32, RFC 5869 max) | `stdlib.crypto.kdf.hkdf_length_exceeds_max` |

A contract violation surfaces as a diagnostic, never a magic correction — the
no-silent-fallback principle. These bounds are pinned by
[test_kdf.caap](../../stdlib/lib/tests/test_kdf.caap).
