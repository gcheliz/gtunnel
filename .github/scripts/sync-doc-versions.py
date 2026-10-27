"""Rewrite the version strings the docs quote to the version being released.

The docs are README.md, docs/usage.md and the landing page, site/index.html.
The release train runs this after release-plz has bumped Cargo.toml, so the
bump commit carries the docs with it, its push redeploys the landing page, and
the "Verify documentation versions"
step in ci.yml holds on the very next pull request. Usage:

    python3 -I .github/scripts/sync-doc-versions.py <version>

It exits 0 whether or not anything changed.
"""

import re
import sys
from pathlib import Path


def sync(text: str, version: str, kind: str) -> str:
    # The landing page marks each version it quotes, so nothing else on it
    # that happens to look like a version can be rewritten by accident
    if kind == "site":
        return re.sub(r"(data-gtunnel-version>)[0-9.]+(<)", rf"\g<1>{version}\g<2>", text)
    text = re.sub(r'VERSION="v[0-9.]+"', f'VERSION="v{version}"', text)
    if kind == "usage":
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
    for name, kind in (
        ("README.md", "readme"),
        ("docs/usage.md", "usage"),
        ("site/index.html", "site"),
    ):
        path = Path(name)
        before = path.read_text()
        after = sync(before, version, kind)
        if after != before:
            path.write_text(after)
            print(f"{name}: synced to {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
