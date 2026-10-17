# Auto-approval constitution templates

These are starting samples for the rule-based auto-approval constitution described in
`docs/design/supervisor-review-and-constitution.md`. They define the target format. The
`kind = "fs"` rules are what `ta-policy` parses today. The `wiki` and `task` kinds, `id`, `mode`,
`min_confidence`, `schema_version` and the layering are the proposed extension (phases
v0.17.12.26 and v0.17.12.27) and are ignored by today's parser, so the files are safe to commit.

Layers: one org file plus one file per project. The project adds to the org file and never
supersedes it. Every sample starts in `mode = "shadow"`: the supervisor decides, a human still
approves, and agreement is measured before a rule is promoted to `enforce`.

| File | Layer | Covers |
|---|---|---|
| `org-amplifiedbyai.toml` | org | wiki updates, shared blocks |
| `project-trustedautonomy.toml` | project | TA repo drafts, wiki, tasks |
| `project-wayfinder.toml` | project | Wayfinder repo drafts, wiki, tasks |
| `project-virtual-team.toml` | project | VT repo drafts, wiki, tasks |

Copy a project file to `.ta/constitution.toml` (or merge its `[[approval_rules]]` into the
existing file). Changes to a constitution go through draft review (`ta constitution amend`),
never silent edits.

Rule semantics (unchanged): first match wins within a file, the most restrictive action across
all items wins, unmatched items go to `review`. Added: across layers the most restrictive
action wins, a project can add rules and tighten but never loosen or disable an org rule, and the
supervisor can only tighten a rule's outcome, never loosen it.
