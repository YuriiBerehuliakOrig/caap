# `tests/`: Test Inputs, Not Examples

Files in this directory are negative or support fixtures consumed by Rust
integration tests. Many are intentionally invalid: missing imports, duplicate
module names, bad types, failing assertions, malformed syntax, or rejected
surface constructs.

Some valid files exist only as the working half of a fixture pair, such as a
module imported by a negative test.

User-facing examples live in [`../examples/`](../examples/).

The in-language stdlib test runner scans `stdlib/lib/` and `examples/`, but it
does not scan this directory. Rust test harnesses that load these fixtures live
under [`../caap/tests/`](../caap/tests/), because Cargo discovers integration
tests there.
