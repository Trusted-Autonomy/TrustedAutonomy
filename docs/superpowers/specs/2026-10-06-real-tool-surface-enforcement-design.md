# Real Tool-Surface Enforcement at Agent Launch

**Status:** drafted for user review. Resolves the gap found while implementing the CoS read-only chat-mode design (`docs/superpowers/specs/2026-10-06-cos-read-only-chat-mode-design.md`).

## The gap, confirmed directly against real code

Two independent, already-tested enforcement planes exist in TA today:

1. **MCP-tool-call content gating** (`ta_policy::PolicyEngine`/`CapabilityManifest`): genuinely real, wired into `ta-mcp-gateway`'s own tool handlers (`ta_fs_read`/`ta_fs_write`, confirmed by PR #633-635's own tests). This governs what a call to one of TA's own MCP tools is allowed to touch, once that tool is invoked.
2. **Native tool-launch gating** (`.claude/settings.local.json`, written by `inject_claude_settings_with_security`, `apps/ta-cli/src/commands/run.rs:6594`): also genuinely real, enforced by the Claude Code CLI itself, not prompt text. This governs which tools (`Bash`, `Read`, `Write`, `Edit`, any MCP server) the launched agent process can use at all.

The gap is that plane 2 is completely disconnected from persona configuration. `DEFAULT_ALLOWED_TOOLS` (`run.rs:6525`) is a single hardcoded constant applied to every launch regardless of persona, and it already includes `Bash(*)`, `Read(*)`, `Write(*)`, `Edit(*)`, `MultiEdit(*)`. `PersonaCapabilities.allowed_tools`/`forbidden_tools` (`crates/ta-goal/src/persona.rs`) exist in the config schema but are never read by the launch path at all: confirmed every real persona file in this repo (`implementer.toml`, `planner.toml`, `reviewer.toml`, `advisor.toml`) has them empty, and `persona.rs`'s only real consumer of these fields is `to_claude_md_section()`, which renders them as descriptive text in the agent's own CLAUDE.md. An agent that ignores that text, or is prompt-injected past it, has nothing technically stopping it from using a tool the text claims is forbidden.

This means today, "CoS holds zero mutating tools" (the chat-mode design's core security property) is real only for calls that happen to go through TA's MCP gateway. If CoS's launched process also has Bash or a native file tool, as every launch does by default, nothing stops it from using that instead.

## The fix: make `allowed_tools`/`forbidden_tools` real, with a configurable ceiling, without creating fragility

Per explicit user direction: the default tool posture should be a configurable setting, but each persona's own declaration must be explicit and must not silently drift if that default changes.

**1. Persona's `allowed_tools` becomes the real allow-list, verbatim, when declared.**

At the one real production call site (`run.rs:3409`, inside `execute()`, which already has `persona_name: Option<&str>` in scope), load the persona (already done nearby at `run.rs:3298` for CLAUDE.md injection) and pass its `capabilities.allowed_tools` into `inject_claude_settings_with_security`. When non-empty, this list is used **instead of** `DEFAULT_ALLOWED_TOOLS`, not merged with it: an explicit declaration is authoritative, never diluted by whatever the default happens to be. When empty (today's vestigial state, matching every existing persona file and preserving current behavior for anyone who hasn't opted in yet), fall back to the posture-level default (next point).

Persona `allowed_tools` entries use Claude Code's own permission-pattern syntax verbatim (`"Bash(*)"`, `"mcp__ta__ta_fs_read"`, `"mcp__ta__ta_wiki_search"`), the same strings that already appear in `DEFAULT_ALLOWED_TOOLS` and in a real `settings.json`. No translation layer between this and `ta_policy`'s own manifest vocabulary (`fs_read`, `fs_write_patch`) is introduced: these stay two deliberately separate enforcement planes (native-launch gating here, MCP-call-content gating in `PolicyEngine`), not merged into one vocabulary.

**2. The posture-level default (today's `DEFAULT_ALLOWED_TOOLS`) becomes configurable per security level, used only as the no-persona-declared fallback.**

Extend `SecurityProfile` (`crates/ta-goal/src/security.rs:168`, which already varies several fields per level in `from_level()`) with `default_allowed_tools: Vec<String>`, populated per level: `Low` keeps today's broad `DEFAULT_ALLOWED_TOOLS` unchanged (no behavior change for existing solo-developer usage), `Mid`/`High` can define a narrower baseline if desired. This is the "configurable default posture" half of the user's direction: it only ever applies when a persona hasn't declared its own list.

**3. The posture's `forbidden_tool_patterns` (already real, already wired via `extra_deny`) stays an unconditional ceiling, applied on top of any persona's declared list.**

No change needed to this mechanism: `extra_deny` already applies regardless of what's in the allow-list, and Claude Code's own permission model denies-over-allow on conflict. A persona explicitly declaring `Bash(*)` still gets `Mid`/`High`'s `DEFAULT_MID_FORBIDDEN_TOOLS` (`Bash(*rm -rf*)`, `Bash(*sudo *)`, etc.) denied underneath it. Raising the posture level can only ever narrow what's possible; it can never widen a persona past what it explicitly declared.

**4. New, optional: a hard allow-ceiling for defense in depth, not just deny-patterns.**

Deny-patterns catch known-dangerous subpatterns but can't cap an over-broad persona declaration to a hard maximum (a persona that declares `Bash(*)` by misconfiguration still gets broad Bash access minus specific denied subpatterns). Add `max_allowed_tools: Option<Vec<String>>` to `SecurityProfile`, unset for `Low`/`Mid`, and settable (empty by default, opt-in) for `High`. When set, the enforced allow-list is `persona.allowed_tools ∩ posture.max_allowed_tools`, not just `persona.allowed_tools` with deny-patterns subtracted. This is additive defense in depth, not required for the base fix to work, and not a regression risk for `Low`/`Mid` since it stays unset there.

## What this gives chat-mode specifically

Once this lands, the CoS persona (and the renamed Curator persona, per the other design doc, both `ta-virtual-team`-side) can declare an explicit `allowed_tools` list limited to `mcp__ta__ta_fs_read`, `mcp__ta__ta_fs_diff`, `mcp__ta__ta_wiki_search`, `mcp__ta__ta_wiki_get`, `mcp__ta__ta_whiteboard_*`, with no `Bash`/`Read`/`Write`/`Edit`/`MultiEdit` entries at all, and that declaration becomes a real, harness-enforced boundary, not documentation. This closes the actual gap `agentic-pm-ba` found, and makes "CoS holds zero mutating tools, ever" true in practice, not just in the design doc's prose.

## Scope and division of labor

TA-core (this repo, in scope for direct implementation):
- `crates/ta-goal/src/security.rs`: add `default_allowed_tools`/`max_allowed_tools` to `SecurityProfile` and `SecurityOverrides`.
- `apps/ta-cli/src/commands/run.rs`: thread persona `allowed_tools` into `inject_claude_settings_with_security` at the real call site (`run.rs:3409`), update the function's allow-list construction to use it when present, apply the optional `max_allowed_tools` intersection.

`ta-virtual-team`-side (dispatched to `agentic-pm-ba`, out of this session's working-directory scope):
- Populate `chief-of-staff.toml`'s `allowed_tools` with the narrow MCP-only list once the TA-core mechanism lands.
- Populate the renamed Curator persona's `allowed_tools` similarly (its wiki tools plus whatever its expanded authoring scope needs).

## Self-review

- Placeholder scan: none. Every mechanism names the real file/line it touches and the real existing field it extends.
- Internal consistency: the "explicit persona declaration, never diluted by a default" requirement and the "default posture is configurable" requirement don't conflict, because the default only ever applies as a fallback when no persona declaration exists, and the posture's deny-ceiling (and optional allow-ceiling) apply unconditionally on top of either case.
- Scope: this doc fixes the native-launch-gating mechanism only. It does not touch `ta_policy::PolicyEngine`/`CapabilityManifest` (already real, already correct for its own plane) and does not introduce a translation layer between the two planes' vocabularies, which would add complexity without closing a real gap.
