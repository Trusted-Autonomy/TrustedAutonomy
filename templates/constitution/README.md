# Auto-approval constitution templates

These are starting samples for the rule-based auto-approval constitution described in
`docs/design/supervisor-review-and-constitution.md`. `ta-policy` parses all of it: the `fs`,
`wiki` and `task` kinds, `id`, `mode`, `min_confidence`, `origins`, `schema_version` and the
org/project layering (see "Constitution v2" in `docs/USAGE.md`). An unknown kind or key is a
loud error, not a skipped rule.

Layers: one org file plus one file per project. The project adds to the org file and never
supersedes it. Every sample starts in `mode = "shadow"`: the supervisor decides, a human still
approves, and agreement is measured before a rule is promoted to `enforce`.

| File | Layer | Covers |
|---|---|---|
| `org-amplifiedbyai.toml` | org | wiki updates, shared blocks |
| `project-trustedautonomy.toml` | project | TA repo drafts, wiki, tasks |
| `project-wayfinder.toml` | project | Wayfinder repo drafts, wiki, tasks |
| `project-virtual-team.toml` | project | VT repo drafts, wiki, tasks |

Copy the org file to `.ta/constitution-org.toml` and a project file to `.ta/constitution.toml` (or merge its `[[approval_rules]]` into the
existing file). Changes to a constitution go through draft review (`ta constitution amend`),
never silent edits.

Rule semantics (unchanged): first match wins within a file, the most restrictive action across
all items wins, unmatched items go to `review`. Added: across layers the most restrictive
action wins, a project can add rules and tighten but never loosen or disable an org rule, and the
supervisor can only tighten a rule's outcome, never loosen it.
