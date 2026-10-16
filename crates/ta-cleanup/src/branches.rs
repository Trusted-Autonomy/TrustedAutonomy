//! The `branches` group: local branches and worktrees whose work is finished.
//!
//! Ground truth is the PR state from the VCS adapter. `git branch -d` and `git cherry` are not
//! used to decide anything: a squash merge rewrites patch ids, so a finished branch looks
//! unmerged to both. Git itself is only asked about commit ancestry and worktree cleanliness.
//!
//! A branch is deleted without asking only when all of these hold:
//! - it is not the default branch, not checked out in the main checkout, and not the checkout
//!   `ta` is running in;
//! - its PR is not open;
//! - it has no commits beyond a merged PR's head, or its tip is already part of the default
//!   branch;
//! - if a worktree has it checked out, that worktree has no uncommitted or untracked files.
//!
//! Everything else is a question (or, for an open PR, a kept branch) and is never auto-deleted.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::*;
use crate::scan::{mk, Scan};

/// Files listed in a question before "and N more".
const FILE_LIST_CAP: usize = 20;

/// Run git in `dir`. Returns trimmed stdout on success, stderr text on failure.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("could not run git in {}: {e}", dir.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn is_ancestor(dir: &Path, maybe_ancestor: &str, of: &str) -> bool {
    // Exit 0 = ancestor (or equal), 1 = not, anything else = unknown object. All but 0 are "no".
    Command::new("git")
        .args(["merge-base", "--is-ancestor", maybe_ancestor, of])
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn commit_exists(dir: &Path, sha: &str) -> bool {
    git(dir, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok()
}

struct Worktree {
    path: PathBuf,
    /// `refs/heads/<name>` stripped, `None` when detached or bare.
    branch: Option<String>,
    locked: bool,
}

fn worktrees(root: &Path) -> Vec<Worktree> {
    let Ok(text) = git(root, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            out.extend(cur.take());
            cur = Some(Worktree {
                path: PathBuf::from(p),
                branch: None,
                locked: false,
            });
        } else if let Some(w) = cur.as_mut() {
            if let Some(b) = line.strip_prefix("branch ") {
                w.branch = b.strip_prefix("refs/heads/").map(str::to_string);
            } else if line == "locked" || line.starts_with("locked ") {
                w.locked = true;
            }
        }
    }
    out.extend(cur);
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `origin/HEAD`'s branch, else `main`/`master` (local or on origin), else `None`.
fn default_branch(root: &Path) -> Option<String> {
    if let Ok(r) = git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        if let Some(b) = r.strip_prefix("origin/") {
            return Some(b.to_string());
        }
    }
    ["main", "master"].iter().find_map(|b| {
        let local = format!("refs/heads/{b}");
        let remote = format!("refs/remotes/origin/{b}");
        (git(root, &["rev-parse", "--verify", "--quiet", &local]).is_ok()
            || git(root, &["rev-parse", "--verify", "--quiet", &remote]).is_ok())
        .then(|| b.to_string())
    })
}

fn short_sha(s: &str) -> &str {
    &s[..s.len().min(10)]
}

/// Uncommitted and untracked files in a worktree. `Err` when git cannot say, which is treated
/// as dirty: an unreadable worktree is never removed.
fn dirty_files(path: &Path) -> Result<Vec<String>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let out = git(path, &["status", "--porcelain", "--untracked-files=all"])?;
    Ok(out.lines().map(|l| l.to_string()).collect())
}

pub(crate) fn branches(s: &Scan, out: &mut Vec<Finding>) {
    let root = &s.e.paths.root;
    if git(root, &["rev-parse", "--git-dir"]).is_err() {
        // Not a git checkout: nothing to prune. Other VCS adapters have no branch group yet.
        return;
    }
    let Some(default) = default_branch(root) else {
        tracing::warn!(path = %root.display(), "cleanup: no default branch found (no origin/HEAD, main or master); skipping the branches group");
        return;
    };
    let default_refs: Vec<String> = [
        format!("refs/heads/{default}"),
        format!("refs/remotes/origin/{default}"),
    ]
    .into_iter()
    .filter(|r| git(root, &["rev-parse", "--verify", "--quiet", r]).is_ok())
    .collect();
    let default_tips: Vec<String> = default_refs
        .iter()
        .filter_map(|r| git(root, &["rev-parse", r]).ok())
        .collect();

    let wts = worktrees(root);
    // The first entry is the main checkout; its branch can never be removed from under it.
    let main_wt = wts.first();

    let refs = match git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%09%(objectname)",
            "refs/heads",
        ],
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(path = %root.display(), error = %e, "cleanup: could not list branches");
            return;
        }
    };
    let mut local: Vec<(String, String)> = refs
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(n, t)| (n.to_string(), t.to_string()))
        .collect();
    local.sort();
    let local_names: HashSet<String> = local.iter().map(|(n, _)| n.clone()).collect();

    for (name, tip) in &local {
        let id = format!("branch:{name}");
        let title = format!("branch {name} ({})", short_sha(tip));
        let recover = format!("git branch {name} {tip}");
        let mut ev = vec![Evidence::new(
            "git",
            format!("tip {tip}; recover with `{recover}`"),
        )];

        let wt = wts.iter().find(|w| w.branch.as_deref() == Some(name));
        let is_main_checkout = main_wt.is_some_and(|m| m.branch.as_deref() == Some(name));
        let is_current = wt.is_some_and(|w| same_path(&w.path, root));
        if *name == default || is_main_checkout || is_current {
            let why = if *name == default {
                "it is the default branch"
            } else if is_current {
                "it is the branch checked out where `ta` is running"
            } else {
                "it is checked out in the main checkout"
            };
            out.push(mk(
                Kind::Branch,
                id,
                title,
                ev,
                Verdict::Skip,
                format!("kept: {why}"),
                Action::None,
                &[("tip", tip.clone()), ("why", "protected".into())],
            ));
            continue;
        }

        // 1. What the PR says.
        let pr = s.e.truth.branch_pr(name);
        let mut merged_heads: Vec<(String, Option<String>)> = Vec::new();
        let mut open: Option<String> = None;
        let mut closed = 0usize;
        let pr_class: &str;
        match &pr {
            BranchPr::Unknown(why) => {
                ev.push(Evidence::new("vcs", format!("PR state unknown: {why}")));
                pr_class = "unknown";
            }
            BranchPr::NoPr => {
                ev.push(Evidence::new("vcs", "no PR was opened from this branch"));
                pr_class = "none";
            }
            BranchPr::Prs(prs) => {
                for p in prs {
                    let head = p.head_sha.as_deref().map(short_sha).unwrap_or("unknown");
                    match &p.state {
                        ReviewState::Open => open = Some(p.review_id.clone()),
                        ReviewState::Merged => {
                            merged_heads.push((p.review_id.clone(), p.head_sha.clone()))
                        }
                        ReviewState::Closed => closed += 1,
                        ReviewState::Unknown(_) => {}
                    }
                    ev.push(Evidence::new(
                        "vcs",
                        format!("PR #{} is {:?}, head {head}", p.review_id, p.state),
                    ));
                }
                pr_class = if open.is_some() {
                    "open"
                } else if !merged_heads.is_empty() {
                    "merged"
                } else if closed > 0 {
                    "closed"
                } else {
                    "unknown"
                };
            }
        }
        if let Some(n) = &open {
            out.push(mk(
                Kind::Branch,
                id,
                title,
                ev,
                Verdict::Skip,
                format!("kept: PR #{n} is still open"),
                Action::None,
                &[("tip", tip.clone()), ("pr", "open".into())],
            ));
            continue;
        }

        // 2. What git says about commits.
        let in_default = default_refs.iter().any(|r| is_ancestor(root, tip, r));
        // A branch sitting exactly on the default tip is indistinguishable from one created a
        // minute ago. Only a PR that really merged makes it finished.
        let on_default_tip = default_tips.iter().any(|t| t == tip);
        let mut beyond: Option<String> = None;
        let mut head_missing = false;
        let within_merged_head = merged_heads.iter().any(|(_, h)| match h {
            Some(h) if commit_exists(root, h) => {
                if is_ancestor(root, tip, h) {
                    true
                } else {
                    if beyond.is_none() {
                        beyond = git(root, &["rev-list", "--count", &format!("{h}..{tip}")]).ok();
                    }
                    false
                }
            }
            _ => {
                head_missing = true;
                false
            }
        });
        if in_default {
            ev.push(Evidence::new(
                "git",
                format!("tip is already part of {default}"),
            ));
        }

        let (safe, why_not): (Option<String>, String) = if within_merged_head {
            (
                Some("its PR is merged and the branch has no commits beyond the PR head".into()),
                String::new(),
            )
        } else if in_default && !on_default_tip {
            (
                Some(format!(
                    "its tip is an ancestor of {default}, so nothing is lost"
                )),
                String::new(),
            )
        } else if in_default && !merged_heads.is_empty() {
            (
                Some(format!("its PR is merged and its tip is part of {default}")),
                String::new(),
            )
        } else if !merged_heads.is_empty() {
            let why = match (&beyond, head_missing) {
                (Some(n), _) if n != "0" => {
                    format!("the PR is merged but the branch has {n} commit(s) beyond the PR head")
                }
                (_, true) => "the PR is merged but its head commit is not in this repository, so the branch cannot be compared (run `git fetch`)".into(),
                _ => "the PR is merged but the branch has commits beyond the PR head".into(),
            };
            (None, why)
        } else {
            let why = match pr_class {
                "closed" => "its PR was closed without merging; the work may be abandoned or may not".to_string(),
                "none" => "no PR was opened from this branch and its commits are not in the default branch".to_string(),
                _ => "the PR state could not be determined and its commits are not in the default branch".to_string(),
            };
            (None, why)
        };

        // 3. Worktree cleanliness.
        let mut dirty: Vec<String> = Vec::new();
        let mut wt_note: Option<String> = None;
        if let Some(w) = wt {
            ev.push(Evidence::new(
                "git",
                format!("checked out in worktree {}", w.path.display()),
            ));
            match dirty_files(&w.path) {
                Ok(f) => dirty = f,
                Err(e) => wt_note = Some(format!("could not read the worktree status: {e}")),
            }
            if w.locked {
                wt_note = Some("the worktree is locked".into());
            }
            if !dirty.is_empty() {
                let mut shown: Vec<String> = dirty.iter().take(FILE_LIST_CAP).cloned().collect();
                if dirty.len() > FILE_LIST_CAP {
                    shown.push(format!("... and {} more", dirty.len() - FILE_LIST_CAP));
                }
                ev.push(Evidence::new(
                    "worktree",
                    format!(
                        "{} uncommitted or untracked file(s):\n      {}",
                        dirty.len(),
                        shown.join("\n      ")
                    ),
                ));
            }
        }
        let wt_path = wt.map(|w| w.path.clone());
        let mut facts = vec![
            ("tip", tip.clone()),
            ("pr", pr_class.to_string()),
            ("safe", safe.is_some().to_string()),
        ];
        if !dirty.is_empty() {
            facts.push(("dirty", dirty.join("\n")));
        }

        let prune = |force: bool, reason: String| Action::PruneBranch {
            branch: name.clone(),
            tip_sha: tip.clone(),
            worktree: wt_path.clone(),
            force,
            reason,
        };
        let manual = match &wt_path {
            Some(p) => format!(
                "git worktree remove {} && git branch -D {name}",
                p.display()
            ),
            None => format!("git branch -D {name}"),
        };

        let mut f = match safe {
            Some(why) if dirty.is_empty() && wt_note.is_none() => {
                let mut f = mk(
                    Kind::Branch,
                    id,
                    title,
                    ev,
                    Verdict::AutoSafe,
                    format!(
                        "{why}{}",
                        wt.map(|_| "; its worktree is clean").unwrap_or("")
                    ),
                    prune(false, why.clone()),
                    &facts,
                );
                f.manual_command = Some(manual);
                f
            }
            other => {
                let reason = match (&other, &wt_note, dirty.is_empty()) {
                    (_, Some(n), _) => format!("{n}; not removing it"),
                    (Some(why), None, false) => format!(
                        "{why}, but its worktree has {} uncommitted or untracked file(s); commit or discard them, or choose below",
                        dirty.len()
                    ),
                    (None, None, false) => format!(
                        "{why_not}, and its worktree has {} uncommitted or untracked file(s)",
                        dirty.len()
                    ),
                    _ => why_not.clone(),
                };
                let mut f = mk(
                    Kind::Branch,
                    id,
                    title,
                    ev,
                    Verdict::Ask,
                    reason,
                    Action::None,
                    &facts,
                );
                f.options = vec![
                    AnswerOption {
                        key: "keep".into(),
                        label: "keep the branch and its worktree".into(),
                        action: Action::None,
                    },
                    AnswerOption {
                        key: "delete".into(),
                        label: format!(
                            "delete the branch{} (tip {} stays recoverable)",
                            if wt_path.is_some() {
                                " and remove its worktree, discarding any uncommitted or untracked files"
                            } else {
                                ""
                            },
                            short_sha(tip)
                        ),
                        action: prune(true, "a person chose to delete it".into()),
                    },
                ];
                f.recommended = Some("keep".into());
                f.manual_command = Some(manual);
                f
            }
        };
        f.bytes = 0;
        out.push(f);
    }

    remote_findings(s, &default, &local_names, out);
}

/// Remote branches whose PR was closed without merging. Reported, never deleted: the repo has
/// `delete_branch_on_merge`, and a closed PR's branch may still be wanted.
fn remote_findings(s: &Scan, default: &str, local: &HashSet<String>, out: &mut Vec<Finding>) {
    let root = &s.e.paths.root;
    let closed: HashSet<String> = s.e.truth.closed_unmerged_branches().into_iter().collect();
    if closed.is_empty() {
        return;
    }
    let Ok(refs) = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%09%(objectname)",
            "refs/remotes/origin",
        ],
    ) else {
        return;
    };
    let mut remote: Vec<(String, String)> = refs
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(n, t)| {
            n.strip_prefix("origin/")
                .map(|b| (b.to_string(), t.to_string()))
        })
        .filter(|(b, _)| b != "HEAD" && b != default && closed.contains(b))
        .collect();
    remote.sort();
    for (name, tip) in remote {
        let also_local = local.contains(&name);
        let mut f = mk(
            Kind::RemoteBranch,
            format!("remote-branch:{name}"),
            format!("remote branch origin/{name} ({})", short_sha(&tip)),
            vec![
                Evidence::new("vcs", "its PR was closed without merging"),
                Evidence::new("git", format!("origin/{name} is at {tip}")),
            ],
            Verdict::Ask,
            format!(
                "PR closed unmerged; origin/{name} still exists{}. Finding only: cleanup never deletes remote branches",
                if also_local { " and a local branch of the same name exists" } else { "" }
            ),
            Action::None,
            &[("tip", tip.clone()), ("pr", "closed".into())],
        );
        f.options = vec![AnswerOption {
            key: "keep".into(),
            label: "leave it (nothing is deleted)".into(),
            action: Action::None,
        }];
        f.recommended = Some("keep".into());
        f.manual_command = Some(format!("git push origin --delete {name}"));
        out.push(f);
    }
}

/// Run a `PruneBranch`: remove the worktree (never forced unless a person chose it), re-check
/// that the branch still points at the scanned tip, delete it and write the recovery log.
pub(crate) fn prune(
    e: &Engine,
    branch: &str,
    tip_sha: &str,
    worktree: Option<&Path>,
    force: bool,
    reason: &str,
) -> anyhow::Result<String> {
    let root = &e.paths.root;
    let current = git(root, &["rev-parse", &format!("refs/heads/{branch}")]).map_err(|_| {
        anyhow::anyhow!("branch {branch} no longer exists; nothing to delete (it was at {tip_sha})")
    })?;
    if current != tip_sha {
        anyhow::bail!(
            "branch {branch} moved from {tip_sha} to {current} since the scan, so it was not deleted. Run `ta cleanup --only branches --dry-run` to look again."
        );
    }
    if let Some(wt) = worktree {
        if wt.exists() {
            let p = wt.to_string_lossy().to_string();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&p);
            git(root, &args).map_err(|err| {
                anyhow::anyhow!(
                    "could not remove worktree {}: {err}. Branch {branch} was kept. Commit or discard the files there, or run `git worktree remove --force {}`.",
                    wt.display(),
                    wt.display()
                )
            })?;
        }
        // A directory that is already gone leaves a stale registration behind.
        let _ = git(root, &["worktree", "prune"]);
    }
    // Compare-and-delete on the scanned tip, so a commit made after the scan is never lost.
    git(
        root,
        &["update-ref", "-d", &format!("refs/heads/{branch}"), tip_sha],
    )
    .map_err(|err| anyhow::anyhow!("could not delete branch {branch}: {err}"))?;
    // `git branch -D` also drops tracking config; update-ref does not.
    let _ = git(
        root,
        &["config", "--remove-section", &format!("branch.{branch}")],
    );

    let recover = format!("git branch {branch} {tip_sha}");
    let entry = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "action": "prune_branch",
        "branch": branch,
        "tip_sha": tip_sha,
        "worktree": worktree.map(|w| w.display().to_string()),
        "forced": force,
        "reason": reason,
        "recover": recover,
    });
    if let Err(err) = crate::state::append_jsonl(&log_path(&e.paths), &entry) {
        tracing::warn!(branch, tip_sha, error = %err, "cleanup: deleted the branch but could not write .ta/cleanup/branches.jsonl");
    }
    tracing::info!(branch, tip_sha, worktree = ?worktree, "cleanup: pruned branch");
    Ok(format!(
        "deleted branch {branch} (tip {tip_sha}){}; recover with `{recover}`",
        worktree
            .map(|w| format!(", removed worktree {}", w.display()))
            .unwrap_or_default()
    ))
}

/// Append-only record of every pruned branch, with the tip needed to bring it back.
pub fn log_path(p: &Paths) -> PathBuf {
    p.cleanup_dir().join("branches.jsonl")
}
