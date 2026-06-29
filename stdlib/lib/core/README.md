# Stdlib Core Library

`lib/core/` holds small, broadly reusable helpers that are independent of loader,
type-pass, frontend, backend, and host-service policy.

Use this directory for:

- equality and structural comparison;
- math, bits, floats, and functional helpers;
- prelude re-exports.

Keep modules dependency-light. If a helper needs diagnostics, host services,
passes, or native codegen state, it belongs in a more specific domain.
