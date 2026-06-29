#!/usr/bin/env python3
"""sync.py — реконсиляція активних скілів воркспейса з канонічними з .agents/.

.agents/ — ДЖЕРЕЛО ПРАВДИ. Цей скрипт читає .agents/manifest.json і:
  1) матеріалізує канонічні скіли/агенти у форму, яку АВТО-ВАНТАЖИТЬ Claude Code
     (.claude/skills/<name>/SKILL.md, .claude/agents/<name>.md);
  2) порівнює з тим, що вже активне у воркспейсі, і ВИВОДИТЬ дрейф (в який бік);
  3) за --apply приводить активні скіли у відповідність до .agents/ (канон).

Режими:
  (без прапора) / --check   лише звіт про дрейф (нічого не пише). exit!=0 якщо дрейф.
  --apply                   створює відсутнє; розбіжне оновлює ЛИШЕ з --force.
  --force                   дозволити перезапис розбіжних активних файлів (деструктивно).
  --prune                   разом з --apply --force: видалити активні скіли/агенти,
                            яких нема в маніфесті.

Матеріалізація = верховий frontmatter + тіло, де КОЖНЕ відносне markdown-посилання
переписано так, щоб коректно резолвитись із нового (глибшого) розташування файлу.
Залежностей поза стандартною бібліотекою немає.
"""
from __future__ import annotations
import argparse
import json
import os
import re
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
AGENTS_DIR = os.path.join(REPO_ROOT, ".agents")
MANIFEST = os.path.join(AGENTS_DIR, "manifest.json")

LINK_RE = re.compile(r"\]\(([^)]+)\)")


def rewrite_links(body: str, src_file_abs: str, dest_file_abs: str) -> str:
    """Переписати відносні markdown-посилання з src-розташування у dest-розташування.

    Кожен відносний таргет резолвиться у реальний шлях (відносно теки src-файлу),
    тоді виражається відносно теки dest-файлу через relpath. Абсолютні/URL/anchor —
    без змін.
    """
    src_dir = os.path.dirname(src_file_abs)
    dest_dir = os.path.dirname(dest_file_abs)

    def repl(m: re.Match) -> str:
        target = m.group(1)
        if target.startswith(("http://", "https://", "mailto:", "/", "#")):
            return m.group(0)
        path, _, anchor = target.partition("#")
        if not path:
            return m.group(0)
        abs_fs = os.path.normpath(os.path.join(src_dir, path))
        new = os.path.relpath(abs_fs, dest_dir).replace(os.sep, "/")
        if anchor:
            new = f"{new}#{anchor}"
        return f"]({new})"

    return LINK_RE.sub(repl, body)


def materialize(src_rel: str, dest_abs: str) -> str:
    """Канонічний вміст .agents/<src_rel>, готовий до запису в dest_abs."""
    src_abs = os.path.join(AGENTS_DIR, src_rel)
    with open(src_abs, encoding="utf-8") as f:
        body = f.read()
    return rewrite_links(body, src_abs, dest_abs)


def dest_for(kind: str, name: str, targets: dict) -> str:
    if kind == "skill":
        return os.path.join(REPO_ROOT, targets["skills"], name, "SKILL.md")
    return os.path.join(REPO_ROOT, targets["agents"], f"{name}.md")


def load_manifest() -> dict:
    with open(MANIFEST, encoding="utf-8") as f:
        return json.load(f)


# ── статуси ──────────────────────────────────────────────────────────────
SYNCED, MISSING, DRIFT = "synced", "missing", "drift"
MARK = {SYNCED: "  ok ", MISSING: "  +  ", DRIFT: "  ~  "}


def plan(manifest: dict):
    """Повертає список (kind, name, status, src_rel, dest_abs, want) + extras."""
    targets = manifest["targets"]
    entries = []
    for kind, key in (("skill", "skills"), ("agent", "agents")):
        for item in manifest.get(key, []):
            name, src_rel = item["name"], item["file"]
            dest_abs = dest_for(kind, name, targets)
            want = materialize(src_rel, dest_abs)
            if not os.path.exists(dest_abs):
                status = MISSING
            else:
                with open(dest_abs, encoding="utf-8") as f:
                    status = SYNCED if f.read() == want else DRIFT
            entries.append((kind, name, status, src_rel, dest_abs, want))

    known_skills = {i["name"] for i in manifest.get("skills", [])}
    known_agents = {i["name"] for i in manifest.get("agents", [])}
    extras = []
    skills_root = os.path.join(REPO_ROOT, targets["skills"])
    if os.path.isdir(skills_root):
        for d in sorted(os.listdir(skills_root)):
            if os.path.isfile(os.path.join(skills_root, d, "SKILL.md")) and d not in known_skills:
                extras.append(("skill", d, os.path.join(skills_root, d)))
    agents_root = os.path.join(REPO_ROOT, targets["agents"])
    if os.path.isdir(agents_root):
        for fn in sorted(os.listdir(agents_root)):
            if fn.endswith(".md") and fn[:-3] not in known_agents:
                extras.append(("agent", fn[:-3], os.path.join(agents_root, fn)))
    return entries, extras


def report(entries, extras):
    print(f"\n.agents/ → активні скіли воркспейса (канон: {AGENTS_DIR})\n")
    counts = {SYNCED: 0, MISSING: 0, DRIFT: 0}
    for kind, name, status, _src, dest_abs, _want in entries:
        counts[status] += 1
        rel = os.path.relpath(dest_abs, REPO_ROOT)
        note = {SYNCED: "в синку", MISSING: "відсутній в активних → буде створено",
                DRIFT: "РОЗБІЖНІСТЬ: активний ≠ канон → буде перезаписано (--force)"}[status]
        print(f"  [{MARK[status]}] {kind:5} {name:22} {rel}\n           {note}")
    for kind, name, path in extras:
        rel = os.path.relpath(path, REPO_ROOT)
        print(f"  [  ?  ] {kind:5} {name:22} {rel}\n           ЗАЙВИЙ: активний є, в маніфесті нема (--prune видалить)")
    print(f"\nПідсумок: synced={counts[SYNCED]} missing={counts[MISSING]} "
          f"drift={counts[DRIFT]} extras={len(extras)}")
    return counts, len(extras)


def apply(entries, extras, force: bool, prune: bool):
    written = skipped = pruned = 0
    for _kind, name, status, _src, dest_abs, want in entries:
        if status == SYNCED:
            continue
        if status == DRIFT and not force:
            print(f"  SKIP (розбіжність, треба --force): {os.path.relpath(dest_abs, REPO_ROOT)}")
            skipped += 1
            continue
        os.makedirs(os.path.dirname(dest_abs), exist_ok=True)
        with open(dest_abs, "w", encoding="utf-8") as f:
            f.write(want)
        verb = "CREATE" if status == MISSING else "OVERWRITE"
        print(f"  {verb}: {os.path.relpath(dest_abs, REPO_ROOT)}")
        written += 1
    if prune:
        if not force:
            print("  --prune потребує --force (деструктивно) — пропущено.")
        else:
            for _kind, _name, path in extras:
                if os.path.isdir(path):
                    for root, _dirs, files in os.walk(path, topdown=False):
                        for fn in files:
                            os.remove(os.path.join(root, fn))
                        os.rmdir(root)
                else:
                    os.remove(path)
                print(f"  PRUNE: {os.path.relpath(path, REPO_ROOT)}")
                pruned += 1
    print(f"\nЗастосовано: written={written} skipped={skipped} pruned={pruned}")
    return skipped


def main() -> int:
    ap = argparse.ArgumentParser(description="Синк активних скілів з канонічними .agents/.")
    ap.add_argument("--check", action="store_true", help="лише звіт (дефолт)")
    ap.add_argument("--apply", action="store_true", help="реконсилювати канон → активні")
    ap.add_argument("--force", action="store_true", help="дозволити перезапис розбіжних")
    ap.add_argument("--prune", action="store_true", help="видалити активні поза маніфестом (з --force)")
    args = ap.parse_args()

    manifest = load_manifest()
    entries, extras = plan(manifest)
    counts, n_extras = report(entries, extras)

    if not args.apply:
        drift = counts[DRIFT] + counts[MISSING]
        if drift:
            print("\nДрейф виявлено. Запусти `python3 .agents/sync.py --apply` "
                  "(для перезапису розбіжних додай --force).")
        else:
            print("\nУсе в синку.")
        return 1 if drift or n_extras else 0

    print("\n--apply:")
    skipped = apply(entries, extras, args.force, args.prune)
    return 1 if skipped else 0


if __name__ == "__main__":
    sys.exit(main())
