#!/usr/bin/env bash
# test-custody-decision.sh: tests scripts/custody-decision.sh with a fake
# `codesign` on PATH and a tempdir HOME. Never touches the real Keychain,
# `ta`, or the real home directory. Run: bash scripts/test-custody-decision.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/custody-decision.sh
source "$ROOT/scripts/custody-decision.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin"
export TA_INSTALL_UNAME=Darwin
unset TA_USER_STATE_DIR

fail=0
check() { # name, condition-exit-code
    if [[ "$2" -eq 0 ]]; then echo "ok   - $1"; else echo "FAIL - $1"; fail=1; fi
}

# fake_codesign TEAM_LINE: installs a `codesign` that prints TEAM_LINE on stderr.
fake_codesign() {
    cat >"$TMP/bin/codesign" <<EOF
#!/usr/bin/env bash
echo "Identifier=com.trustedautonomy.ta" >&2
echo "$1" >&2
EOF
    chmod +x "$TMP/bin/codesign"
}
fresh_home() { export HOME="$TMP/home-$1"; rm -rf "$HOME"; mkdir -p "$HOME"; }
PATH="$TMP/bin:$PATH"

# 1. No Team ID, no custody file -> writes `file`, dir 0700, file 0600.
fake_codesign "TeamIdentifier=not set"; fresh_home 1
out="$(ta_decide_custody /fake/ta)"
f="$HOME/.config/ta/human/custody"
[[ "$(cat "$f" 2>/dev/null)" == "file" ]]; check "no Team ID writes 'file'" $?
[[ "$(ls -ld "$f" | cut -c1-10)" == "-rw-------" ]]; check "custody file is 0600" $?
[[ "$(ls -ld "$(dirname "$f")" | cut -c1-10)" == "drwx------" ]]; check "custody dir is 0700" $?
grep -q "Team ID: none" <<<"$out"; check "prints Team ID status (none)" $?
grep -q "ta credentials custody keychain" <<<"$out" && grep -q "TA_CODESIGN_IDENTITY" <<<"$out"; check "prints how to opt into the keychain" $?
grep -q "cdhash" <<<"$out"; check "prints why (cdhash)" $?

# 2. No Team ID, an explicit `keychain` file exists -> never overwritten.
fresh_home 2; mkdir -p "$HOME/.config/ta/human"; printf 'keychain\n' >"$HOME/.config/ta/human/custody"
out="$(ta_decide_custody /fake/ta)"
[[ "$(cat "$HOME/.config/ta/human/custody")" == "keychain" ]]; check "existing custody file is never overwritten" $?
grep -q "untouched" <<<"$out"; check "says it left the existing choice alone" $?

# 3. Team ID present -> no custody file.
fake_codesign "TeamIdentifier=ABCDE12345"; fresh_home 3
out="$(ta_decide_custody /fake/ta)"
[[ ! -e "$HOME/.config/ta/human/custody" ]]; check "Team ID present writes no custody file" $?
grep -q "Team ID: ABCDE12345" <<<"$out"; check "prints Team ID status (present)" $?

# 4. TA_USER_STATE_DIR is honoured.
fake_codesign "TeamIdentifier=not set"; fresh_home 4
TA_USER_STATE_DIR="$TMP/custom" ta_decide_custody /fake/ta >/dev/null
[[ "$(cat "$TMP/custom/custody" 2>/dev/null)" == "file" && ! -e "$HOME/.config" ]]; check "TA_USER_STATE_DIR is honoured" $?

# 5. Non-macOS: does nothing.
fresh_home 5
TA_INSTALL_UNAME=Linux ta_decide_custody /fake/ta >/dev/null
[[ ! -e "$HOME/.config" ]]; check "non-macOS writes nothing" $?

# 6. Unsigned binary (codesign prints no TeamIdentifier) counts as no Team ID.
cat >"$TMP/bin/codesign" <<'EOF'
#!/usr/bin/env bash
echo "code object is not signed at all" >&2
exit 1
EOF
fresh_home 6
ta_decide_custody /fake/ta >/dev/null
[[ "$(cat "$HOME/.config/ta/human/custody" 2>/dev/null)" == "file" ]]; check "unsigned binary writes 'file'" $?

exit "$fail"
