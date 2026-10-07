#!/usr/bin/env bash
# local-env.sh: read one setting from the gitignored .env.local at the repo
# root. Sourced by install_local.sh and codesign-runner.sh. The file is parsed,
# never executed: only simple KEY=value lines (optional quotes, # comments).
#
# Usage (after sourcing): ta_local_env KEY DEFAULT
# Order: real environment variable, then .env.local, then DEFAULT.
ta_local_env() {
    local key="$1" default="$2" val
    val="${!key:-}"
    if [[ -z "$val" ]]; then
        local root file
        root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
        file="$root/.env.local"
        if [[ -f "$file" ]]; then
            val="$(sed -n "s/^[[:space:]]*\(export[[:space:]]\{1,\}\)\{0,1\}${key}=//p" "$file" | tail -1)"
            val="${val%\"}"; val="${val#\"}"; val="${val%\'}"; val="${val#\'}"
        fi
    fi
    printf '%s' "${val:-$default}"
}
