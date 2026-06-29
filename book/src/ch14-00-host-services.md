# Host Services and the System Runtime

A program that only computes values is of limited use; eventually it must read a
file, print a line, spawn a process, or talk to the network. In CAAP these are
**host services**: capabilities the runtime provides and the launcher grants.
This chapter surveys them and the safety model around them. They're the same
services whether you run on the evaluator or compile to native — the native
backend just reaches them through the runtime's C ABI (Chapter 13).

## The `sys.*` Facades

Host services are grouped into modules under `sys.*`. Each is a thin facade over
the runtime; you `use` the ones you need.

| Module | What it offers |
|---|---|
| `sys.io` | `print`, `println`, `eprint`/`eprintln`, `write`, `read_line`, `read_all`, `flush_stdout` |
| `sys.fs` | file open/read/write/close, directory listing, metadata, links, remove |
| `sys.os` | environment, working directory, platform queries |
| `sys.process` | spawn and manage subprocesses |
| `sys.net` | TCP `listen`/`connect`/`accept`/`read`/`write`/`close`, and UDP |
| `sys.time` | clocks and timestamps |

The full function lists are in the [kernel reference](appendix-04-further-reading.md),
section 19. A facade's callable wrappers are partial applications of the
underlying runtime operation — for example `println` is "the runtime `println`,
already bound to the io handle," so you call `(println "hi")`, never
`(println <handle> "hi")`.

## Capabilities: Asking for Authority

Every host service requires a **capability**, and capabilities come from the
launcher (Chapter 9). The bare kernel grants none — that's why `(println …)` is
`unknown name` there. The tower form

```bash
caap stdlib/bootstrap.caap PROGRAM
```

runs the bootstrap with the `sys` capability, which is what lets `sys.io`,
`sys.fs`, and the rest bind their wrappers. Without the grant a `sys.*` module
still *loads* — its wrappers are self-describing throwing stubs, so importing it
never fails; calling one without the grant raises "requires a sys grant" rather
than acting. Capabilities are requested by name; the obsolete blanket
`host_services` alias is rejected by the kernel's capability normaliser, so you
name the authority you actually need.

## The Sandbox: Policies and Roots

Authority isn't all-or-nothing. The host installs a **policy** that narrows what
granted capabilities may touch. For the file system, the policy carries
**read roots** and **write roots** — allow-lists of directories:

- Empty write roots means "deny all writes."
- `None` means "unrestricted."
- A request path is canonicalised and must live under one of the roots, or the
  operation is denied with a diagnostic.

This is the *compile-time sandbox*: when the compiler folds your code or runs a
pass (Chapter 11), that code executes under a policy that confines its file
access, and — together with the fatal allocation and depth budgets from
Chapter 6 — cannot escape, exhaust memory, or wander the file system. Native
executables you build, by contrast, run with the ambient authority of the
process that launched them, like any other program.

## Reading Input

A program that reads a line and echoes it back, in outline:

```scheme
(use sys.io println read_line)

(bind line (read_line))          ; returns the line (including its newline)
(println (string_concat_many "you said: " (string_trim line)))
```

`read_all` slurps standard input to EOF — handy for filters. File and network
reads follow the same shape: open a handle, read/write, close. (Recall from
Chapter 6 that caller-supplied read sizes are bounded by the runtime, so a hostile
size can't make the host allocate without limit.)

## One Runtime, Two Front Doors

The key design point: there is **one** implementation of every system operation,
in the `caap-sys-runtime` crate, reached two ways. The interpreter calls it
directly. A native binary calls the `extern "C"` exports of `caap-sys-runtime-ffi`
(the `caap_runtime_*` symbols you saw in the LLVM IR). Same behaviour, same handle
tables, same policy gate — so a program behaves identically interpreted or
compiled. That consistency is why the HTTP-server demo in the corpus produces the
same result both ways.

With host services in hand, you can write programs that do real work. Let's build
one.
