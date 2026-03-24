# LinkedIn Post — Trusted Autonomy Public Beta

---

We just shipped the public beta of **Trusted Autonomy** (`public-beta-v0.13.17.7`) and I wanted to share what it actually does, because "AI agent framework" undersells the specific problem it solves.

**The problem**: AI coding agents are powerful but opaque. You hand one a task, it goes off and edits files, runs commands, maybe touches things it shouldn't — and you find out after the fact. The bigger the task, the harder it is to verify what changed and why.

**What TA does**: Every agent action is staged, diffed, and requires your explicit approval before anything touches your real codebase. The agent works in an isolated copy. You review a structured diff — with an AI summary of what changed and why — then approve, deny, or selectively apply individual files.

**What's new in this release:**

🔒 **Supervisor agent** — After your main agent finishes but before you review the diff, a second AI (configurable: Claude Code, Codex, or local Ollama) independently reviews the changes against your stated goal and your team's constitution. It flags scope creep, unrelated edits, or constitution violations before you see the draft. Runs automatically. Fallback to warn if it can't run — never blocks a build.

🗂 **VCS isolation** — Spawned agents get their own isolated git environment (their own `.git`, `GIT_CEILING_DIRECTORIES` set). No more index.lock collisions, no accidental commits to your real repo from inside the staging copy. Configurable: `isolated` (default), `inherit-read`, or `none`.

🛡 **Gitignored artifact gate** — TA-injected config files (`.mcp.json`, etc.) are stripped from diffs before review. Gitignored build artifacts that slip into a changeset trigger a human review gate rather than silently aborting a `git add`.

🔑 **Auth flexibility** — Supervisor works with subscription-based Claude Code (no API key needed), API key, or local models. No more `ANTHROPIC_API_KEY` hard requirement for users on Pro/Max plans.

📦 **Release bundle** — macOS, Linux, and Windows installers now include the Perforce VCS plugin and offline usage docs.

---

**How to jump in:**

```bash
# macOS/Linux
curl -fsSL https://github.com/Trusted-Autonomy/TrustedAutonomy/releases/latest/download/install.sh | sh

# Then: set up a project and run your first goal
ta init
ta run "add input validation to the user registration endpoint"
# Review the diff, approve what looks good
ta draft view
ta draft approve <id>
ta draft apply <id> --git-commit
```

The [usage guide](https://github.com/Trusted-Autonomy/TrustedAutonomy/releases/latest/download/USAGE.html) covers the full workflow — plan-linked goals, multi-agent swarms, constitution files, and the interactive shell.

It's local-first, Rust, open-source. No cloud dependency, no telemetry, works with any agent that speaks Claude Code, Codex, or Ollama.

Would love feedback from anyone building with AI agents day-to-day — especially on the supervisor review quality and what violation types it catches (or misses) in practice.

---

*#AIEngineering #DeveloperTools #OpenSource #RustLang #CodingAgents*
