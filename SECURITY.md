# Security Policy

CAAP is a compiler and runtime, so its security story is mostly about
**capability and effect containment**: what code that runs under the compiler or
the produced binary is allowed to touch. This document describes that boundary,
the supported versions, and how to report a vulnerability.

This is an early-stage, pre-1.0 project. The threat model below is real and
enforced today, but the project does not yet make stability or backport
guarantees (see *Supported versions*).

## The security boundary

CAAP has **no ambient `read_file`** — and no ambient `write_file`, `open_socket`,
or any other free-floating host primitive. Every host service (`io`, `fs`, `os`,
`process`, `net`, `time`, `rand`, `path`) is reached only through typed `sys.*`
facades backed by a **capability handle**, and a handle can only be *minted*
inside a bootstrap frame that was granted the matching capability. (See the
"Effects & capabilities" section of [`CLAUDE.md`](CLAUDE.md) and principle #8 in
[`docs/principles.md`](docs/principles.md): system APIs are explicit modules, not
ambient globals.)

The full policy is documented in
[`docs/mechanisms/stdlib-sys-grant-policy.md`](docs/mechanisms/stdlib-sys-grant-policy.md).
The properties that matter for security:

- **Capability is off by default.** A bare compiler session knows nothing and
  holds no authority. Host access exists only after
  [`stdlib/boot/sys_grants.caap`](stdlib/boot/sys_grants.caap) is run
  *explicitly* with the umbrella `sys` capability — that is the one file that
  mints host authority. A program loaded without it (or never handed any
  registry value from it) has no path to the host: its `sys.*` facades still
  type-check, but *calling* an op raises `… requires a sys grant …` rather than
  silently returning. This is the project's no-silent-fallback rule applied to
  authority.

- **A handle, not a name string, is authority.** Holding the capability value is
  what grants access; knowing the name of an op is not. The only place a handle
  is minted is inside the granted `sys_grants.caap` frame.

- **Privilege can only ever narrow.** `effect_scope` dynamically attenuates
  privileges around untrusted callbacks (and installs an allocation budget for
  that scope). Nested scopes can only narrow, never regain — a callback running
  under an attenuated scope cannot climb back up to the umbrella capability.

- **Embedders can hand out *less*, never more.** Restricted embeddings use
  `grant_for` to give a component only the ops it needs (down to a single
  ready-to-call op with the handle already applied away), instead of the whole
  umbrella grant. `grant_for` cannot manufacture authority: with no grant
  registered it returns `null`.

What this is **not**: a sandbox for arbitrary native machine code. A program
compiled to a native binary, or an embedder that runs `sys_grants.caap` and
shares the registry with untrusted code, is as privileged as the process it runs
in. The capability boundary constrains CAAP-level effects within a session, not
the host OS.

If you believe any of the boundary properties above can be bypassed — e.g. a
`sys.*` op callable without a grant, a way to re-read or re-mint a handle from an
attenuated scope, or `effect_scope` failing to narrow — please report it as a
vulnerability.

## Supported versions

CAAP is pre-release. Only the latest `main` is developed and patched; there are
no released, backport-supported lines yet.

| Version | Status                | Security fixes |
| ------- | --------------------- | -------------- |
| `0.1.x` (main) | Active, pre-release | Best effort, on `main` only — no stability or backport guarantees yet |

When the project reaches a stable release, this table will be updated with the
versions that receive security backports.

## Reporting a vulnerability

**Please do not open a public GitHub issue for a security vulnerability.** Report
it privately so it can be triaged and fixed before disclosure.

1. **Preferred: GitHub private Security Advisories.** Go to the repository's
   **Security** tab and choose **"Report a vulnerability"**. This opens a private
   advisory thread visible only to you and the maintainers.

2. **Fallback: email.** If you cannot use GitHub Security Advisories, email
   **yurii.berehuliak@gmail.com**.

Please include enough to reproduce: the affected component (core, stdlib,
sys/capability layer, CLI, …), a minimal program or steps, the observed vs.
expected behavior, and the commit you tested. A proof-of-concept showing the
capability boundary being crossed is especially helpful.

You can expect an acknowledgement of your report. As a pre-release, best-effort
project there is no formal SLA yet; fixes land on `main`, and we will coordinate
disclosure timing with you.
