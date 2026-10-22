#!/usr/bin/env python3
"""Store the plan-usage part of Claude Code's statusline JSON for `ta usage`.

Claude Code passes a JSON object on stdin to the user's statusline command.
Chain this script from that command and it writes the plan fields to a
snapshot file that `ta usage` reads. Same result as `ta usage capture`, with
no TA binary needed.

What it does:
  * reads one JSON object from stdin,
  * keeps only rate_limits, rate_limits_available and subscription_type,
  * stamps captured_at (UTC) and writes the file atomically (temp file in the
    same directory, then rename), so a reader never sees half a file.

What it never does: read credentials, OAuth tokens or the Keychain, call any
network endpoint, or print to stdout (a statusline command's stdout becomes
the status line, so this script must stay silent there). Errors go to stderr
and the exit code is non-zero; chain it with `|| true` so a failure never
breaks the statusline.

Snapshot path: --out FILE, else $TA_USAGE_DIR/plan-usage.json, else
~/.config/ta/usage/plan-usage.json. Python 3.8+, standard library only.
"""
import argparse
import datetime
import json
import os
import sys
import tempfile

KEEP = ("rate_limits", "rate_limits_available", "subscription_type")


def snapshot_path(out):
    if out:
        return out
    base = os.environ.get("TA_USAGE_DIR")
    if not base:
        base = os.path.join(os.path.expanduser("~"), ".config", "ta", "usage")
    return os.path.join(base, "plan-usage.json")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out", help="snapshot file to write")
    args = ap.parse_args()

    raw = sys.stdin.read()
    try:
        data = json.loads(raw)
    except ValueError as e:
        sys.stderr.write(
            "ta-usage-capture: stdin is not valid JSON (%s). Expected the object Claude Code "
            "passes to a statusline command; see docs/USAGE.md, 'Usage and budget'.\n" % e
        )
        return 2
    if not isinstance(data, dict):
        sys.stderr.write("ta-usage-capture: stdin JSON is not an object; nothing written.\n")
        return 2

    snap = {
        "schema": 1,
        "provider": "claude_code",
        "captured_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "statusline": {k: data[k] for k in KEEP if k in data},
    }
    path = snapshot_path(args.out)
    directory = os.path.dirname(os.path.abspath(path))
    try:
        os.makedirs(directory, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=directory, prefix=".plan-usage.", suffix=".tmp")
        try:
            with os.fdopen(fd, "w") as f:
                json.dump(snap, f, indent=2)
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except OSError as e:
        sys.stderr.write(
            "ta-usage-capture: could not write %s (%s). Check the directory is writable or "
            "set TA_USAGE_DIR / --out.\n" % (path, e)
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
