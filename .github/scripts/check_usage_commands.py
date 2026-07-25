#!/usr/bin/env python3
"""Check that every built-in slash command is documented in USAGE.md.

Source of truth: the SLASH_COMMAND_SPECS registry in
rust/crates/commands/src/lib.rs. A command counts as documented when its
name (or one of its aliases) appears in USAGE.md as `/name`.

Usage:
  python3 .github/scripts/check_usage_commands.py          # fail on undocumented
  python3 .github/scripts/check_usage_commands.py --list   # print inventory, never fail
"""
from __future__ import annotations

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = ROOT / 'rust' / 'crates' / 'commands' / 'src' / 'lib.rs'
USAGE = ROOT / 'USAGE.md'

# Commands intentionally left out of USAGE.md (internal/experimental), plus
# the ratchet baseline: commands that predate this checker and are not yet
# documented. Burn the baseline down over time — and never add a NEW command
# here: document it in USAGE.md instead. The check fails only on regressions
# beyond this set.
ALLOW_UNDOCUMENTED = [
    "add-dir", "advisor", "agent", "alias", "allowed-tools",
    "api-key", "approve", "autofix", "benchmark", "blame",
    "brief", "budget", "build", "changelog", "chat",
    "commit", "cron", "debug-tool-call", "definition", "deny", "desktop",
    "diagnostics", "docs", "env", "exit", "explain", "feedback", "fix",
    "format", "git", "hover", "ide", "image", "insights",
    "issue", "language", "lint", "listen", "log", "macro", "map",
    "max-tokens", "metrics", "migrate", "multi", "notifications",
    "output-style", "parallel", "paste", "perf", "plugin", "pr",
    "profile", "project", "providers", "rate-limit", "reasoning", "refactor",
    "references", "rename", "reset", "run",
    "screenshot", "search", "share", "speak", "stash", "stickers",
    "subagent", "symbols", "system-prompt", "tag", "team",
    "telemetry", "temperature", "templates", "terminal-setup", "test",
    "thinkback", "tool-details",
    "vim", "voice", "workspace",
]


def parse_registry(text: str) -> list[tuple[str, list[str]]]:
    """Return (name, aliases) for each spec in SLASH_COMMAND_SPECS."""
    match = re.search(
        r'const SLASH_COMMAND_SPECS[^=]*=\s*&\[(.*?)^\];',
        text,
        re.DOTALL | re.MULTILINE,
    )
    if not match:
        print(f'error: SLASH_COMMAND_SPECS block not found in {REGISTRY}', file=sys.stderr)
        sys.exit(2)
    specs = []
    for m in re.finditer(
        r'SlashCommandSpec\s*\{\s*name:\s*"([^"]+)"\s*,\s*aliases:\s*&\[([^\]]*)\]',
        match.group(1),
    ):
        name = m.group(1)
        aliases = re.findall(r'"([^"]+)"', m.group(2))
        specs.append((name, aliases))
    if not specs:
        print(f'error: no SlashCommandSpec entries parsed from {REGISTRY}', file=sys.stderr)
        sys.exit(2)
    return specs


def documented_commands(text: str) -> set[str]:
    """Names mentioned as /name in USAGE.md.

    File paths are excluded: no word/path character may precede the slash
    (rules out `./x`, `a/b`, `~/.claw/x`) and the name must not be followed
    by another path segment (rules out `/tmp/x`, `/v1/chat`).
    """
    return set(re.findall(r'(?<![\w./\\-])/([a-z][a-z0-9_-]*)(?![\w/-])', text))


def main() -> int:
    list_only = '--list' in sys.argv[1:]

    specs = parse_registry(REGISTRY.read_text(encoding='utf-8'))
    documented = documented_commands(USAGE.read_text(encoding='utf-8'))

    missing = []
    for name, aliases in sorted(specs):
        if name in documented or any(alias in documented for alias in aliases):
            continue
        if name in ALLOW_UNDOCUMENTED:
            continue
        missing.append(name)

    if list_only:
        print(f'registry commands ({len(specs)}):')
        for name, aliases in sorted(specs):
            status = 'documented' if name not in missing else 'MISSING'
            if name in ALLOW_UNDOCUMENTED:
                status = 'allowed-undocumented'
            alias_note = f' (aliases: {", ".join(aliases)})' if aliases else ''
            print(f'  /{name:<28} {status}{alias_note}')
        print(f'\ndocumented in USAGE.md: {len(specs) - len(missing)}/{len(specs)}')
        return 0

    if missing:
        print('usage command check failed: commands registered in '
              f'{REGISTRY.relative_to(ROOT)} but not mentioned in USAGE.md:', file=sys.stderr)
        for name in missing:
            print(f'  - /{name}', file=sys.stderr)
        print(f'\n{len(missing)} undocumented command(s). Document them in USAGE.md '
              'or add them to ALLOW_UNDOCUMENTED in this script.', file=sys.stderr)
        return 1

    print(f'usage command check passed ({len(specs)} registry commands all documented)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
