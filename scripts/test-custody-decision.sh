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
# fake_security EXIT: installs a `security` whose find-generic-password exits EXIT
# (0 = a TA item exists, 1 = none), so no test reads the real keychain.
fake_security() {
    printf '#!/usr/bin/env bash\nexit %s\n' "$1" >"$TMP/bin/security"
    chmod +x "$TMP/bin/security"
}
fake_security 1
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

# 7. No Team ID, no custody file, but the keychain already holds TA secrets ->
# do NOT switch (a switch without migration would mint replacement keys).
fake_codesign "TeamIdentifier=not set"; fake_security 0; fresh_home 7
out="$(ta_decide_custody /fake/ta)"
[[ ! -e "$HOME/.config/ta/human/custody" ]]; check "keychain holding TA secrets blocks the automatic switch" $?
grep -q "ta credentials custody file" <<<"$out" && grep -q "did NOT switch" <<<"$out"; check "tells the user to migrate explicitly" $?

# 8. The rule is owned by `ta state user-dir`: when BINARY answers with an
# absolute path, that wins over the mirrored HOME rule. (A fake `ta` script;
# the real one is never run.)
fake_codesign "TeamIdentifier=not set"; fake_security 1; fresh_home 8
cat >"$TMP/bin/fake-ta" <<EOF
#!/usr/bin/env bash
[[ "\$1 \$2" == "state user-dir" ]] && printf '%s\n' "$TMP/from-ta" && exit 0
exit 2
EOF
chmod +x "$TMP/bin/fake-ta"
out="$(ta_decide_custody "$TMP/bin/fake-ta")"
[[ "$(cat "$TMP/from-ta/custody" 2>/dev/null)" == "file" && ! -e "$HOME/.config" ]]; check "'ta state user-dir' decides the directory when available" $?

# 9. A `ta` that predates the subcommand (non-zero, prints nothing usable) or
# answers with a relative path falls back to the documented precedence.
fresh_home 9
printf '#!/usr/bin/env bash\necho "error: unrecognized subcommand" >&2\nexit 2\n' >"$TMP/bin/old-ta"
chmod +x "$TMP/bin/old-ta"
ta_decide_custody "$TMP/bin/old-ta" >/dev/null
[[ "$(cat "$HOME/.config/ta/human/custody" 2>/dev/null)" == "file" ]]; check "old ta falls back to the documented rule" $?
fresh_home 10
printf '#!/usr/bin/env bash\necho relative/dir\n' >"$TMP/bin/odd-ta"
chmod +x "$TMP/bin/odd-ta"
( cd "$TMP" && ta_decide_custody "$TMP/bin/odd-ta" >/dev/null )
[[ "$(cat "$HOME/.config/ta/human/custody" 2>/dev/null)" == "file" && ! -e "$TMP/relative" ]]; check "a relative answer from ta is rejected" $?

# 11. TA_USER_STATE_DIR is honoured by the fallback too, and with no HOME, no
# override and no usable ta nothing is written (never a cwd-relative path).
fresh_home 11
mkdir -p "$TMP/cwd11"
out="$( cd "$TMP/cwd11" && unset HOME TA_USER_STATE_DIR && ta_decide_custody /fake/ta )"
[[ -z "$(ls -A "$TMP/cwd11")" ]]; check "no HOME and no override writes nothing in the cwd" $?
grep -q "cannot tell where TA keeps its per-user state" <<<"$out"; check "no-HOME case says what to do" $?

exit "$fail"
