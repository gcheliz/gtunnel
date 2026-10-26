#!/bin/sh
# Print one version's section of CHANGELOG.md, read from stdin: the lines after
# its `## [<version>]` heading, up to the next `## [` heading. The release body
# is this text verbatim, so the changelog stays the one place notes are written.
# Exits 1 when the heading is missing, so a release never goes out with no notes.
set -eu
version="${1:?usage: release-notes.sh <version> < CHANGELOG.md}"
awk -v want="## [${version}]" '
  index($0, "## [") == 1 {
    if (found) exit
    if (index($0, want) == 1) { found = 1; next }
  }
  found { print }
  END { exit found ? 0 : 1 }
'
