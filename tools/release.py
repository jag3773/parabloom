#!/usr/bin/env python3
"""Release checks used by .github/workflows/release.yml.

Usage:
  python3 tools/release.py version
  python3 tools/release.py check <tag> [--allow-unreleased]
  python3 tools/release.py notes <version> [--allow-unreleased]

`check` confirms the tag matches the version in Cargo.toml and that
CHANGELOG.md has a non-empty section for it. `notes` prints that section.
`--allow-unreleased` falls back to the `[Unreleased]` section, which lets
pull requests dry-run the release before the changelog is finalised.
"""

import argparse
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
SEMVER = re.compile(r"\d+\.\d+\.\d+")
HEADING = re.compile(r"^## \[(?P<name>[^\]]+)\]")


class ReleaseError(Exception):
    pass


def cargo_version(root=ROOT):
    with open(root / "Cargo.toml", "rb") as f:
        return tomllib.load(f)["package"]["version"]


def changelog_section(text, version, allow_unreleased=False):
    """Return the body of the `## [version]` section, stripped."""
    sections = {}
    name = None
    for line in text.splitlines():
        match = HEADING.match(line)
        if match:
            name = match["name"]
            sections[name] = []
        elif line.startswith("[") and "]: " in line:
            # Link reference definitions at the end of the file.
            name = None
        elif name is not None:
            sections[name].append(line)
    body = "\n".join(sections.get(version, [])).strip()
    if not body and allow_unreleased:
        body = "\n".join(sections.get("Unreleased", [])).strip()
    if not body:
        raise ReleaseError(f"CHANGELOG.md has no entries for {version}")
    return body


def check(tag, root=ROOT, allow_unreleased=False):
    if not tag.startswith("v") or not SEMVER.fullmatch(tag[1:]):
        raise ReleaseError(f"tag {tag!r} is not of the form vMAJOR.MINOR.PATCH")
    version = cargo_version(root)
    if tag[1:] != version:
        raise ReleaseError(f"tag {tag} does not match Cargo.toml version {version}")
    text = (root / "CHANGELOG.md").read_text()
    return changelog_section(text, version, allow_unreleased)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("version")
    for name in ("check", "notes"):
        p = sub.add_parser(name)
        p.add_argument("tag" if name == "check" else "version")
        p.add_argument("--allow-unreleased", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.command == "version":
            print(cargo_version())
        elif args.command == "check":
            check(args.tag, allow_unreleased=args.allow_unreleased)
            print(f"{args.tag} is ready to release")
        else:
            text = (ROOT / "CHANGELOG.md").read_text()
            print(changelog_section(text, args.version, args.allow_unreleased))
    except ReleaseError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
