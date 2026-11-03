#!/usr/bin/env bash
# custody-decision.sh: decide the per-user key-custody default for a dev install.
# Sourced by install_local.sh (and by scripts/test-custody-decision.sh).
#
# Why: a macOS keychain item carries a `partition_id` ACL. A binary signed by a
# self-signed local certificate has no Team ID, so macOS identifies it by its
# cdhash, and every rebuild is a new identity for every item: endless prompts.
# The custody file `<user_state_dir>/custody` makes every TA process (ta,
# ta-daemon, the Wayfinder poller) keep secrets in owner-only files instead.
#
# This file must never touch the Keychain and never run `ta`: it only inspects
# a signature with `codesign` and writes one small file.

# ta_user_state_dir: same resolution as ta_credentials::human::user_state_dir.
ta_user_state_dir() {
    if [[ -n "${TA_USER_STATE_DIR:-}" ]]; then
        printf '%s' "$TA_USER_STATE_DIR"
    else
        printf '%s' "${HOME}/.config/ta/human"
    fi
}

# ta_team_identifier BINARY: prints the Team ID, or nothing when the binary has
# none ("not set"), is unsigned, or codesign is unavailable.
ta_team_identifier() {
    local out id
    out="$(codesign -dv --verbose=2 "$1" 2>&1 || true)"
    id="$(printf '%s\n' "$out" | sed -n 's/^TeamIdentifier=//p' | head -1)"
    id="${id%"${id##*[![:space:]]}"}"
    if [[ -z "$id" || "$id" == "not set" ]]; then
        return 0
    fi
    printf '%s' "$id"
}

# ta_keychain_holds_ta_secrets: true when the login keychain already has TA items.
# Metadata-only lookup (no -g/-w), which does not read the secret and does not
# prompt. Switching custody to `file` while a secret exists only in the keychain
# would make the next process mint a replacement key, so the installer must not
# do that on its own: `ta credentials custody file` migrates with a verified copy.
ta_keychain_holds_ta_secrets() {
    command -v security &>/dev/null || return 1
    local svc
    for svc in trusted-autonomy-vault trusted-autonomy-control-plane; do
        if security find-generic-password -s "$svc" &>/dev/null; then
            return 0
        fi
    done
    return 1
}

# ta_decide_custody BINARY: macOS only. Writes `file` to the custody file when
# BINARY has no Team ID and no custody file exists yet. NEVER overwrites an
# existing file (an explicit choice wins). Prints what it decided and why.
# TA_INSTALL_UNAME overrides `uname -s` (for tests).
ta_decide_custody() {
    local binary="$1" os team dir file
    os="${TA_INSTALL_UNAME:-$(uname -s)}"
    if [[ "$os" != "Darwin" ]] || ! command -v codesign &>/dev/null; then
        return 0
    fi
    team="$(ta_team_identifier "$binary")"
    dir="$(ta_user_state_dir)"
    file="$dir/custody"

    if [[ -n "$team" ]]; then
        echo "Signing identity Team ID: $team (keychain grants survive rebuilds; the keychain default stays)."
        return 0
    fi
    echo "Signing identity Team ID: none (cdhash-identified binary: macOS re-prompts for every keychain item on every rebuild)."
    if [[ -e "$file" ]]; then
        echo "Key custody: leaving your existing choice in $file untouched (currently: $(tr -d '[:space:]' <"$file" 2>/dev/null))."
        return 0
    fi
    if ta_keychain_holds_ta_secrets; then
        echo "Key custody: this Mac's keychain already holds Trusted Autonomy secrets, so the installer did NOT switch to file custody"
        echo "  (switching without moving them would make TA create replacement keys)."
        echo "  Migrate once, with a verified copy, then prompts stop for good:  ta credentials custody file"
        echo "  macOS will ask once to let ta read the old items: choose Always Allow."
        return 0
    fi
    if ! (umask 077 && mkdir -p "$dir" && chmod 700 "$dir" && printf 'file\n' >"$file" && chmod 600 "$file"); then
        echo "Warning: could not write $file. Keychain prompts will recur on every rebuild." \
             "Create it yourself with the single word 'file', or run: ta credentials custody file"
        return 0
    fi
    echo "Key custody: wrote 'file' to $file."
    echo "  Why: this binary has no Team ID, so macOS keys keychain access to its cdhash and would"
    echo "       prompt again after every rebuild. File custody keeps secrets in owner-only files instead."
    echo "  Existing keychain secrets are NOT moved automatically. To migrate the live project: ta credentials custody file"
    echo "  To opt back into the keychain: ta credentials custody keychain, or create an Apple Development"
    echo "  certificate and put its name in .env.local as TA_CODESIGN_IDENTITY (it has a Team ID)."
}
