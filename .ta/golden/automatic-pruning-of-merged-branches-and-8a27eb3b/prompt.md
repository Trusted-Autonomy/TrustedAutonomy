# Goal: automatic pruning of merged branches and worktrees

Owner rule: finished branches and worktrees must be pruned every time, without a human asking. Read apps/ta-cli/src/commands/cleanup.rs (the `ta cleanup` engine, groups drafts|staging|goals|locks|archive) and how `ta draft apply --submit`, `ta draft watch` and PR merge detection work.

## Do
1. Add a `branches` group to `ta cleanup`. Ground truth is the PR state from the VCS adapter, NOT `git branch -d` or `git cherry` (squash merges rewrite patch ids). A local branch is SAFE to delete when its PR is merged AND the branch has no commits beyond the PR head, or the branch tip is an ancestor of the default branch. Never delete the current branch, the default branch, or a branch checked out in a worktree with uncommitted or untracked files (report those as questions, with the file list). A branch with an open PR, no PR, or commits beyond the PR head is a QUESTION, never auto-deleted. Also prune worktrees whose branch is done, after the same cleanliness check.
2. Safe deletions run and are logged in the cleanup log with branch name AND tip SHA so it is recoverable; `--dry-run` shows reasons.
3. Run this group automatically (a) right after `ta draft apply` when its PR is observed merged (`ta draft watch` path) and (b) as part of the existing scheduled/startup cleanup if one exists. Make it configurable under the existing cleanup config; default ON.
4. Remote branches: do not delete remotes (the repo has delete_branch_on_merge). Report remote branches whose PR was closed unmerged as findings only.
5. Tests with tempfile::tempdir and a local bare repo as origin; fake PR state through the adapter trait the engine already uses. Cases: merged-squash branch deleted, open PR kept, branch with extra commits asked, dirty worktree asked, current branch never touched. Windows-agnostic paths.
6. docs/USAGE.md how-to under cleanup.

## Rules
- Do NOT edit version strings. No em dashes. Never use git branch -D in tests against the real repo.
- Run build, test, clippy -D warnings, fmt --check and report truthfully. Mark no PLAN phase done.
