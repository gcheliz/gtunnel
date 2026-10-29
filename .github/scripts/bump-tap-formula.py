"""Point the Homebrew formula at the release that was just published.

The formula lives in the tap repository, gcheliz/homebrew-tap. The release
workflow runs this after the release is public, with the four checksum files
the build jobs wrote beside their tarballs. Usage:

    python3 -I .github/scripts/bump-tap-formula.py <vX.Y.Z> <assets-dir> <formula>

The hashes come from those files, never from the network: they are the ones
the release was published with. It exits 0 whether or not anything changed, and
non-zero when the formula or an asset is not what it expects, so a bad bump
never reaches the tap.
"""

import re
import sys
from pathlib import Path

# The same four strings as `platform_slug()` in src/update.rs and the build matrix
SLUGS = ("macos-arm64", "macos-intel", "linux-amd64", "linux-arm64")


def digest(assets: Path, slug: str, tag: str) -> str:
    name = f"gtunnel-{slug}-{tag}.tar.gz"
    path = assets / f"{name}.sha256"
    if not path.is_file():
        raise SystemExit(f"missing {path}")
    # `sha256sum` writes "<hash>  <name>"; anything else is not this release
    match = re.fullmatch(r"([0-9a-f]{64})  \*?" + re.escape(name) + r"\n?", path.read_text())
    if not match:
        raise SystemExit(f"{path} does not hold the checksum of {name}")
    return match.group(1)


def bump(text: str, tag: str, assets: Path) -> str:
    for slug in SLUGS:
        pattern = re.compile(
            r'(url "https://github\.com/gcheliz/gtunnel/releases/download/)'
            rf"v[0-9.]+/gtunnel-{slug}-v[0-9.]+(\.tar\.gz\"\n\s+sha256 \")[0-9a-f]{{64}}(\")"
        )
        text, count = pattern.subn(
            lambda m: f"{m.group(1)}{tag}/gtunnel-{slug}-{tag}{m.group(2)}{digest(assets, slug, tag)}{m.group(3)}",
            text,
        )
        if count != 1:
            raise SystemExit(f"expected one {slug} url and sha256 pair in the formula, found {count}")
    return text


def main() -> int:
    if len(sys.argv) != 4 or not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", sys.argv[1]):
        print("usage: bump-tap-formula.py <vX.Y.Z> <assets-dir> <formula>", file=sys.stderr)
        return 2
    tag, assets, formula = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
    old = formula.read_text()
    new = bump(old, tag, assets)
    if new == old:
        print(f"{formula} already points at {tag}")
        return 0
    formula.write_text(new)
    print(f"{formula} now points at {tag}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
