#!/usr/bin/env bash
# codesign-runner.sh: Cargo `runner` for macOS. Cargo runs every test binary
# and every `cargo run` binary through this script (see .cargo/config.toml).
#
# Why: macOS Keychain "Always Allow" grants are tied to the code-signing
# identity. A freshly built, unsigned test binary is a brand new identity each
# build, so any test that touches the Keychain re-prompts every time. Signing
# each binary with the same stable local identity (and one fixed identifier
# for all test binaries) makes one grant cover every build.
#
# Safe everywhere else: it only signs when the named local identity exists in
# this user's Keychain (the same cert install_local.sh uses). On Linux, CI,
# or any Mac without that cert it does nothing and just runs the binary.
# Never ad-hoc signs. Never fails the run because signing failed.
#
# Override the identity with TA_CODESIGN_IDENTITY (same variable as
# install_local.sh).
set -u
bin="$1"
shift

if [[ "$(uname -s)" == "Darwin" ]] && [[ -x /usr/bin/codesign ]]; then
    identity="${TA_CODESIGN_IDENTITY:-Trusted Autonomy Local Dev}"
    # Cheap check first: skip if already signed by this identity.
    if ! /usr/bin/codesign -dv --verbose=4 "$bin" 2>&1 | grep -qF "Authority=${identity}"; then
        # Fails fast, without touching the file, when the identity is absent.
        /usr/bin/codesign --force --sign "$identity" \
            --identifier "com.trustedautonomy.ta-test" "$bin" >/dev/null 2>&1 || true
    fi
fi

exec "$bin" "$@"
