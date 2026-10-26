"""Rewrite the version strings the docs quote to the version being released.

The release train runs this after release-plz has bumped Cargo.toml, so the
bump commit carries the docs with it and they quote the version being released.
Usage:

    python3 -I .github/scripts/sync-doc-versions.py <version>

It exits 0 whether or not anything changed.
"""

import re
import sys
from pathlib import Path


def sync(text: str, version: str, usage: bool) -> str:
    text = re.sub(r'VERSION="v[0-9.]+"', f'VERSION="v{version}"', text)
    if usage:
        text = re.sub(
            r"(\$ gtunnel --version\n)gtunnel [0-9.]+", rf"\g<1>gtunnel {version}", text
        )
        text = re.sub(r"\(current: v[0-9.]+\)", f"(current: v{version})", text)
    return text


def main() -> int:
    if len(sys.argv) != 2 or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", sys.argv[1]):
        print("usage: sync-doc-versions.py <major.minor.patch>", file=sys.stderr)
        return 2
    version = sys.argv[1]
    for name, usage in (("README.md", False), ("docs/usage.md", True)):
        path = Path(name)
        before = path.read_text()
        after = sync(before, version, usage)
        if after != before:
            path.write_text(after)
            print(f"{name}: synced to {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
