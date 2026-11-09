# Wiki read-side off switch for goal agents

Owner decision (2026-10-15): "wiki OFF" means TA does not let an agent READ the wiki to seed or inform its context. Writes still happen (curator/librarian goals write through draft review). It is a read-side switch only.

Add `[wiki] seed_context = true|false` to `.ta/workflow.toml` (default true, so nothing changes for existing projects).

When false:
1. The TA MCP gateway refuses the wiki READ tools (ta_wiki_search, ta_wiki_get, ta_wiki_types, and any other wiki read path you find, including the wiki cache and any context injection that pulls wiki pages into an agent's CLAUDE.md or prompt) with an error that names the setting, the file, and how to turn it back on. Refusal is in the gateway handler, not only in a prompt, so it is enforceable.
2. Wiki WRITE tools (ta_wiki_create, ta_wiki_update, and the draft-review write path) keep working exactly as before. Add a test proving a write still succeeds while reads are refused.
3. `ta doctor` or `ta status` shows the setting state when it is false (one line), so the owner can see why reads are refused.
4. Chat-mode CoS wiki reads are also refused when false (they go through the same gateway).
5. Read the config the same way other `[section]` settings in workflow.toml are read; reuse the existing loader. Missing section or key means true. Malformed value is an error that names the file and key.

Tests: default true allows reads; false refuses each read tool with the actionable message; false allows writes; malformed value errors with file and key; chat mode refused when false.
Docs: add a short how-to to docs/USAGE.md (no version annotations).
Do not edit PLAN.md or set versions by hand. Observability mandate applies: refusals must say what, why, and what to do.
