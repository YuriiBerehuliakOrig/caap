# `.agents/`: Canonical CAAP Agent And Skill Home

`.agents/` is the repository source of truth for AI-agent support material:
skills, agent roles, playbooks, and shared conventions. Point an agent at this
directory first; it should read this index, load the relevant skill, and, when
needed, sync workspace-active Claude Code skills from this canonical source.

## Format

Skills and agents are Markdown files with YAML frontmatter (`name`,
`description`). That is the shape Claude Code can materialize as
`.claude/skills/<name>/SKILL.md` and `.claude/agents/<name>.md`.

This repository keeps `.agents/` as the canonical source. Active `.claude/`
files are generated from it by [`sync.py`](#sync). The sync index is
[`manifest.json`](manifest.json).

## Structure

```text
.agents/
  README.md         this file
  conventions.md    shared DRY rules
  manifest.json     skill/agent/playbook index and versions
  sync.py           reconcile active workspace skills with the canonical source
  skills/           reusable procedures and guardrails
  agents/           roles that compose skills for a task
  playbooks/        step-by-step recipes for multi-file tasks
```

## Shared Conventions

All shared rules live in [`conventions.md`](conventions.md): editing `.caap`
files through the refactor script, grounding claims in code, preserving default
behavior, minimizing mechanisms, respecting compile-time/runtime phases, using
golden references, committing atomically, understanding project components, and
definition of done.

Skills link to specific sections instead of copying the text.

## Skills

| Skill | Use when | File |
| --- | --- | --- |
| `caap-refactor` | Editing existing `.caap` files with the span-based refactor script. | [`skills/caap-refactor.md`](skills/caap-refactor.md) |
| `build-and-test` | Building, running gates, tests, or the `caap` binary. | [`skills/build-and-test.md`](skills/build-and-test.md) |
| `caap-language` | Understanding CAAP syntax, semantics, primitives, and phase boundaries. | [`skills/caap-language.md`](skills/caap-language.md) |
| `builtins-analysis` | Reviewing whether builtins/CTFE primitives belong in core. | [`skills/builtins-analysis.md`](skills/builtins-analysis.md) |
| `stdlib-optimization` | Choosing the right stdlib mechanism and phase, reducing duplication. | [`skills/stdlib-optimization.md`](skills/stdlib-optimization.md) |
| `docs-generation` | Updating kernel/stdlib reference docs from code. | [`skills/docs-generation.md`](skills/docs-generation.md) |
| `readability-refactor` | Behavior-preserving cleanup of repeated boilerplate. | [`skills/readability-refactor.md`](skills/readability-refactor.md) |

## Agents

| Agent | Purpose | File |
| --- | --- | --- |
| `caap-reviewer` | Review diffs against CAAP conventions and contracts. | [`agents/caap-reviewer.md`](agents/caap-reviewer.md) |
| `stdlib-author` | Author `.caap` kits, helpers, and examples. | [`agents/stdlib-author.md`](agents/stdlib-author.md) |
| `host-runtime-engineer` | Work on Rust builtins, CTFE primitives, and host services. | [`agents/host-runtime-engineer.md`](agents/host-runtime-engineer.md) |
| `docs-writer` | Keep kernel and stdlib reference docs synchronized with code. | [`agents/docs-writer.md`](agents/docs-writer.md) |

## Playbooks

| Playbook | Task | File |
| --- | --- | --- |
| `add-builtin` | Add a runtime builtin. | [`playbooks/add-builtin.md`](playbooks/add-builtin.md) |
| `add-ctfe-primitive` | Add a compile-time CTFE primitive. | [`playbooks/add-ctfe-primitive.md`](playbooks/add-ctfe-primitive.md) |
| `add-host-service` | Add a host/FFI service. | [`playbooks/add-host-service.md`](playbooks/add-host-service.md) |

## Sync

`.agents/` is canonical. Active Claude Code skills and agents are derived. The
sync script reconciles and reports drift. Destructive overwrite requires
`--force`.

```bash
# Report drift only.
python3 .agents/sync.py
python3 .agents/sync.py --check

# Create missing active files.
python3 .agents/sync.py --apply

# Also overwrite divergent active files.
python3 .agents/sync.py --apply --force

# Also remove active files outside the manifest.
python3 .agents/sync.py --apply --force --prune
```

Sync behavior:

1. Read `manifest.json`.
2. Materialize each skill/agent into `.claude/skills/<name>/SKILL.md` or
   `.claude/agents/<name>.md`.
3. Rewrite relative Markdown links with `os.path.relpath`.
4. Report `+` missing, `~` divergent, `?` extra, and `ok` in sync.
5. Apply changes only when requested.

Do not edit `.claude/skills/*` manually; edit `.agents/` and sync.

## Extending

1. Add a Markdown file under [`skills/`](skills/) or [`agents/`](agents/) with
   frontmatter.
2. Add a `manifest.json` entry.
3. Add a row to the relevant table above.
4. Run `python3 .agents/sync.py` and verify the reported drift.

Keep each skill short: when to use it, procedure, guardrails, and links deeper
into the repo. Do not duplicate full `docs/` or `KERNEL_REFERENCE.md` content.
