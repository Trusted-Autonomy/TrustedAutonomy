#!/usr/bin/env bash
# scripts/test-publish-order.sh: guards the crates.io publish order.
#
# Asserts, from `scripts/publish-crates.sh --list-order` and `cargo metadata`:
#   1. every publishable workspace crate is listed exactly once (nothing stale, nothing missing);
#   2. every crate comes after all the workspace crates it depends on through
#      [dependencies] / [build-dependencies] (the rule crates.io enforces);
#   3. every such internal dependency carries a version (crates.io rejects path-only ones).
# No network, no token. Run from anywhere. Exit 0 = ok.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

ORDER="$(./scripts/publish-crates.sh --list-order)"

META="$(mktemp)"; trap 'rm -f "$META"' EXIT
cargo metadata --no-deps --format-version 1 > "$META"

ORDER="$ORDER" META="$META" python3 - <<'PY'
import json, os, sys
order = os.environ["ORDER"].split()
m = json.load(open(os.environ["META"]))
pkgs = {p["name"]: p for p in m["packages"] if p.get("publish") != []}
fail = []
if len(order) != len(set(order)):
    fail.append("a crate is listed twice: " + ", ".join(sorted(n for n in set(order) if order.count(n) > 1)))
stale = sorted(set(order) - set(pkgs))
missing = sorted(set(pkgs) - set(order))
if stale:
    fail.append("listed but not a publishable workspace crate: " + ", ".join(stale))
if missing:
    fail.append("publishable workspace crates missing from the order: " + ", ".join(missing))
pos = {n: i for i, n in enumerate(order)}
for name, p in pkgs.items():
    for d in p["dependencies"]:
        if d["name"] not in pkgs or d["kind"] == "dev":
            continue
        if d["req"] == "*":
            fail.append("%s -> %s has no version" % (name, d["name"]))
        if name in pos and d["name"] in pos and pos[d["name"]] > pos[name]:
            fail.append("%s is published before its dependency %s" % (name, d["name"]))
if fail:
    for f in fail:
        print("test-publish-order: FAIL: " + f)
    sys.exit(1)
print("test-publish-order: ok (%d crates, dependencies always first)" % len(order))
PY
