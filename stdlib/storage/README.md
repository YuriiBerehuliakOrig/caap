# `stdlib.storage` — a binary-format compiler that is just a *library*

`storage/binary.caap` lets you describe an on-disk binary format and have the
**library** — not the kernel — act as a compiler for it: parse the spec, check it
at compile time, and lower it to specialized, zero-overhead encode / decode /
validate / migrate functions. Nothing about any particular format lives in the
kernel; the kernel only provides the generic mechanisms (a form is a compile-time
function, an expander, an IR, a renderer).

This is the *engine* reference: how the `stdlib.storage.binary` DSL turns a record
spec into encode/decode code over the two backends, with byte-exactness asserted by
its own test coverage. The `spec → validate → generate` pipeline is documented below.

## The pipeline: `spec → validate → generate → two backends`

```
(storage …)  ──parse──▶  spec data  ──validate──▶  located diagnostics
                                         │ (clean)
                                         ▼
                                      generate  ──▶  eval IR   (runs on the kernel today)
                                      render_native ─▶ native subset (backend/emit/llvm → real LLVM)
```

`(storage Name …)` is a **library-defined grammar extension** (a `define_form`):
the reader accepts these heads and the library *runs* over the parsed AST. CAAP is
an s-expression language, so the "grammar" is parentheses rather than the braces a
hand-written PEG would add — the compiler-feature part (validation + lowering +
generated code) is identical either way.

### 1. `parse` — spec node → spec data

`parse_storage` walks the `(storage …)` node into a plain map: `endian`,
`block_size`, `crc`, a list of `records` (each with `name`, `version`, `fields`),
and a list of `migrations`. Every field carries its source location, so a later
diagnostic can point at the offending line.

### 2. `validate` — seven compile-time checks (each a located diagnostic)

On a dirty spec the form emits errors and lowers to nothing. The checks:

1. **duplicate field** in a record version
2. **duplicate record version**
3. **unsupported field type** (names the field and its spelling)
4. **enum value size** — a variant value must fit the base, and the base must be
   an **unsigned** int
5. **CRC region** — a `crc_of` field must be `u32` and the **last** field
6. **missing migration** — consecutive versions of a record need a `migrate`
7. **endian** — `(endian X)` must be `little`/`le` or `big`/`be` (an unknown
   spelling is a diagnostic, never a silent fall-through to little-endian)

### 3. `generate` — spec data → generated IR (the visible, runnable output)

A clean spec lowers to a map of specialized functions plus constants and docs:

```text
encode_<R>_v<N>   decode_<R>_v<N>   validate_<R>_v<N>
migrate_<R>_v<A>_to_v<B>          load_<R>_latest
<R>_V<N>_SIZE      docs      snapshot      report
```

The generated code is **real CAAP IR with baked field offsets** (decode reads each
field at a constant offset), so it is specialized per record, not an interpreter
over a runtime layout. It is built as AST trees (`syntax/ast` + `syntax/ir`)
and serialized **once** via `syntax/render` — no hand-spliced source strings.

The eval `decode` is **defensive**: it opens with a length guard (a buffer shorter
than the record's fixed size raises, rather than reading off-the-end zeros), and a
record with a `crc_of` field re-computes the CRC over the region before it and
raises on a mismatch (a corrupt buffer is rejected, not silently accepted). The
native `decode` reads a bare `ptr_u8` (no carried length), so it has neither guard
— the two backends differ in decode-time validation by construction.

## Two backends from one spec: eval and native LLVM

| | eval target (`generate`) | native target (`render_native`) |
|---|---|---|
| value model | dynamic lists / maps; a buffer is a list of bytes | typed `(struct …)` aggregate per record |
| byte ops | the small **byte runtime** below (`le_bytes` / `read_le` / …) | `ptr_add` / `ptr_write` + shift ops |
| runs | on the kernel **today** (round-trip without a C toolchain) | `backend/emit/llvm` → real LLVM / machine code |
| `bytes[N]` | supported (a sub-list) | record **skipped** (an array field is a separate slice) |

Both backends lower the **same** spec; a green eval round-trip plus the native
LLVM emit is the eval = native parity gate (pinned by
`caap/tests/tinylogfs_tests.rs` and `stdlib/lib/tests/test_storage.caap`).

## Supported field types

| spelling | kind | size | eval | native | notes |
|---|---|---|---|---|---|
| `u8` `u16` `u32` `u64` | unsigned int | 1/2/4/8 | yes | yes | byte order per `(endian …)` |
| `i8` `i16` `i32` `i64` | signed int | 1/2/4/8 | yes | yes | two's-complement |
| `bool` | bool | 1 | yes | yes (as a `u8` byte) | decodes back to a boolean |
| `(bytes N)` | fixed bytes | N | yes | record skipped natively | truncate / zero-pad |
| `(enum uN …)` | tagged union | sizeof base | yes | yes | base must be **unsigned** |
| `float` `f64` / `f32` | IEEE-754 | 8 / 4 | yes | yes | via `float_to_bits`/`bits_to_float`; the native field is a `f64`/`f32` bit-cast through an LLVM `bitcast` |

**Signed integers** are two's-complement. Encode needs no special case — the
kernel's `int_mod`/`int_div` are Euclidean, so `(le_bytes -1 1)` already yields the
byte `255`. Decode sign-extends via `read_le_signed` (the eval backend); the native
backend reads the bytes into the accumulator and `cast`s to the field type, where
`backend/emit/llvm` picks `ashr` / `sext` from the signed type. **Bool** occupies a whole
byte (`0`/`1`) — byte-exact on disk, and the native field is a `u8`, so there is no
sub-byte `i1` trap. **Floats** lower on **both** backends: the eval byte runtime
round-trips them through the kernel's `float_to_bits` / `bits_to_float` inverse
bit-casts (8-byte f64, 4-byte f32; the sign bit survives because `le_bytes` is
two's-complement-exact), and the native backend emits the same bit-casts as an LLVM
`bitcast` (the native field is a `f64`/`f32`), so the on-disk bytes are identical —
byte-level eval = native parity (pinned in `caap/tests/tinylogfs_tests.rs`).

## Byte order: `(endian little)` (default) or `(endian big)`

A spec's `(endian …)` directive picks the byte order for every **scalar** field
(ints, enums, bool-as-byte, the float bit pattern, and the CRC field's own 4
bytes). `little`/`le` lower to the LE byte runtime (`le_bytes` / `read_le` /
`read_le_signed`); `big`/`be` lower to the big-endian twins (`be_bytes` /
`read_be` / `read_be_signed`). `(bytes N)` fields are a raw byte slice and are
**byte-order agnostic** (copied verbatim on both). Both backends — eval and
native — honor the directive, so the on-disk bytes match (the native writes pick
the LSB-first vs MSB-first shift). An unrecognized spelling is validate check 7.

## Boundaries (deliberate, documented)

These are not oversights — each is a real limit of the backend, surfaced as a
located diagnostic rather than wrong code.

- **`u64` ≥ 2⁶³ on the eval backend.** The kernel integer is `i64`, so an 8-byte
  field assembles into an `i64` bit pattern: a `u64` with bit 63 set comes back as a
  **negative** `i64` from the eval `decode` (the bytes are still exact, and the
  **native** backend stores a true `u64`). `i64` is unaffected — the pattern *is*
  the signed value. This holds for both `read_le` and `read_be` (same i64-bit-
  pattern contract).
- **`bytes[N]` records skip the native target.** A native struct array field is a
  separate slice, not an aggregate field, so a record containing `(bytes N)` lowers
  only on the eval backend (`render_native` skips it). Int / signed / bool / enum
  records lower on both.

## The byte runtime (eval backend)

The generated eval functions reference these by name, so the unit using `storage`
imports them:

```lisp
(use stdlib.storage.binary
  le_bytes read_le read_le_signed
  be_bytes read_be read_be_signed
  crc32 emit! pad_bytes slice_bytes)
```

| function | purpose |
|---|---|
| `le_bytes value n` | `value` as `n` little-endian bytes (two's-complement for negatives) |
| `read_le bs off n` | the `n`-byte little-endian **unsigned** int at `off` (i64 bit pattern) |
| `read_le_signed bs off n` | like `read_le`, sign-extended to a two's-complement signed int |
| `be_bytes value n` | `value` as `n` **big-endian** bytes (MSB first) |
| `read_be bs off n` | the `n`-byte big-endian **unsigned** int at `off` (i64 bit pattern) |
| `read_be_signed bs off n` | like `read_be`, sign-extended to a two's-complement signed int |
| `pad_bytes lst n` | a fixed-width `n`-byte field (truncate / zero-pad) |
| `slice_bytes bs off n` | `n` bytes of `bs` from `off` |
| `emit! acc lst` | append every byte of `lst` to buffer `acc` |
| `crc32 bs start end` | CRC-32 (poly `0xEDB88320`) over `[start, end)` |

A `(endian big)` spec's generated encode/decode call the `be_*` helpers; the
unit using such a spec imports them (the LE-named twins for `(endian little)`).

## API

`binary.caap` exports the engine entry points for tooling:

```lisp
(export parse_storage validate generate render_native
        le_bytes read_le read_le_signed
        be_bytes read_be read_be_signed
        pow256 crc32 emit! pad_bytes slice_bytes)
```

- `parse_storage node` → spec data
- `validate spec node` → list of located diagnostic strings (empty = clean)
- `generate spec` → `{ pairs:[{k,v-spec}…], report:[line…] }` (eval lowering)
- `render_native spec` → native-subset CAAP source (for `backend/emit/llvm`)
