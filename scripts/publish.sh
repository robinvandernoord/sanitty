#!/bin/bash
# vommit's publish step (see pyproject.toml): the crates of a release, in order.
#
# The karet-lsp fork keeps its own version (0.8.0, the karet-lsp it patches), so
# most releases do not change it: it is published only when crates.io does not
# have that version yet. Then sanitty, which cargo uploads once the fork is in
# the index.
set -euo pipefail
cd "$(dirname "$0")/.."

fork=sanitty-karet-lsp
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "crates/$fork/Cargo.toml" | head -n 1)
[ -n "$version" ] || { echo "publish: no version in crates/$fork/Cargo.toml" >&2; exit 1; }

status=$(curl -s -o /dev/null -w '%{http_code}' -A "sanitty-release" "https://crates.io/api/v1/crates/$fork/$version")
case $status in
    200) echo "publish: $fork $version is on crates.io already" ;;
    404) cargo publish --locked -p "$fork" ;;
    *) echo "publish: crates.io answered $status for $fork $version; not publishing" >&2; exit 1 ;;
esac
cargo publish --locked -p sanitty
