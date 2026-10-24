#!/bin/bash
# Prints the version to stamp on installers: the tag (v1.2.3 -> 1.2.3) when
# building a release, otherwise the Cargo.toml version.
set -euo pipefail
if [[ "${GITHUB_REF:-}" == refs/tags/v* ]]; then
    echo "${GITHUB_REF#refs/tags/v}"
else
    grep -m1 '^version' "$(dirname "$0")/../Cargo.toml" | cut -d'"' -f2
fi
