# Atomic Apply and the Durable Action Queue

**Status:** design from the owner's direction, 2026-10-07. Not built. Supersedes the "re-run apply to retry" model in PR #642.

## 1. The honest answer first: not everything can be rolled back

Applying a draft does two very different kinds of work. Copying files into a project can be undone. Sending an email cannot. A design that promises "atomic, with rollback" for both would be lying about the second kind. This design promises exactly what each kind can deliver, and makes the difference visible.

Every change a draft makes is in one of three classes:

| Class | Meaning | Example |
|---|---|---|
| **Reversible** | Can be undone completely, leaving no trace. | Writing project files. |
| **Compensable** | Can be countered by an opposite action, but the first action still happened and may have been seen. | Posting a Slack message, then deleting it. |
| **Irrecoverable** | Cannot be undone. | Sending an email. |

## 2. What "atomic" promises

1. **The local commit is all or nothing.** All reversible changes (project files, the local git commit, the status flip to Applied) land together or not at all. A crash at any point leaves either the old state or the new one, never a mix.
2. **Remote effects are queued durably and run at most once.** Compensable and irrecoverable actions are written into a durable queue in the same commit step. They run afterwards, survive restarts, and retry safely. They are not part of the all-or-nothing local commit, because nothing can make a remote call all-or-nothing with a local one.
3. **Irrecoverable actions run last, and only after everything else succeeded.** The moment of no return is explicit, visible and as late as possible.
4. **Unknown outcomes are never guessed.** If the process dies mid-send, the action moves to "needs attention". A person decides whether it went out.

## 3. Reversibility by medium and supplier

Each connector declares its own class and its undo, so the class is data and not a guess. If a connector declares nothing, it is treated as **irrecoverable**.

| Medium | Supplier or mechanism | Class | Undo and caveats |
|---|---|---|---|
| Project files | Local filesystem | Reversible | Staged copy and rename, journaled. Restore from the journal. |
| Local git commit | git | Reversible | Reset to the previous commit while unpushed. |
| Push, PR | GitHub, GitLab | Compensable | Close the PR or push a revert. Force-push is never used. History is visible. |
| Task or wiki change | Wayfinder | Compensable | Restore the prior value. The apply step records the prior value first. Wiki also has revision history (`if_sha`). |
| Database changes in a transaction | PostgreSQL | Reversible until commit, then compensable | Roll back before commit. After commit, only inverse statements, which may not exist. |
| Database schema change | PostgreSQL | Reversible (transactional DDL) | Inside a transaction. |
| Database schema change | MySQL | Irrecoverable | DDL commits implicitly and cannot be rolled back. |
| Object storage | S3 with versioning on | Reversible | Restore the previous version. Without versioning: irrecoverable. |
| Email | SMTP, Gmail, SES | Irrecoverable | Recall is unreliable and never promised. |
| Chat message | Slack, Discord | Compensable (weak) | Delete the message. People may already have seen it or been notified. |
| Social post | X, LinkedIn | Compensable (weak) | Delete the post. Copies and screenshots remain. |
| Calendar invite | Google, Microsoft | Compensable | Cancel the event. Invitees are notified. |
| Payment | Stripe | Compensable | Refund or void. Fees and customer visibility remain. Uses an idempotency key. |
| Cloud resource created | AWS, GCP, Azure | Compensable | Delete it. Cost already incurred. |
| Cloud resource deleted | AWS, GCP, Azure | Irrecoverable | Reversible only with soft delete or versioning. |
| Deployment | Any CD system | Compensable | Roll back to the previous release. Traffic already served. |
| Package publish | npm, crates.io | Irrecoverable | Yank hides it from new installs. The version number is burned. |
| Generic REST call | Any | Per connector | Default irrecoverable. |

## 4. The apply sequence

1. **Prepare.** Check everything without changing anything: policy and constitution, path safety, conflicts, and a connector preflight where the supplier supports one (for example "is this recipient valid"). Write a journal that lists every file change and every action, with prior values for anything compensable. Any failure here changes nothing.
2. **Commit.** In one step: move the files in (staged copy, then rename), make the local commit if requested, mark the draft Applied, and write every action into the durable queue. If the process dies, startup recovery reads the journal and either finishes the commit or rolls it back.
3. **Run the queue.** A worker in the daemon drains the queue at startup and continuously. Order: compensable actions first, irrecoverable actions last.
4. **Report.** `ta show actions` lists what is queued, done, retrying, and waiting on a person.

## 5. The action queue

- **Storage:** a daemon-owned location outside anything an agent can write (see red-team finding CR-03), append-only journal plus an index.
- **Key:** draft id plus action id. The same key can never run twice.
- **States:** `queued`, `sending`, `done`, `retrying`, `needs attention`.
- **Retry:** growing delays up to a cap and a maximum attempt count. Policy and the automation constitution are re-checked on every attempt.
- **Needs attention:** reached when an outcome is unknown, policy now blocks the action, or retries run out. Shown to the user with one plain sentence and the one thing to do.
- **Undo for compensable actions:** `ta show actions` offers the recorded undo. Running it is a new, logged action, not a silent rewrite of history.

## 6. Strict mode (optional, per posture)

In the `strict` posture a draft may require **saga order**: compensable remote actions run before the local commit and are compensated automatically in reverse if the commit then fails. This trades speed and complexity for a stronger promise, and still cannot cover irrecoverable actions, which always run last.

## 7. What changes in existing work

PR #642 built the parts to keep: fsynced intent before any send, policy re-check at send time, fail-closed config, per-action isolation, the `ta_propose_*` exclusion. Its "re-run `ta draft apply`" retry path and `--resend` flag are replaced by the queue worker. `ta draft apply` is run once per draft.

## 8. Open decisions

1. Connector manifests gain `reversibility`, `undo` and `idempotency` fields. Existing connectors default to irrecoverable until updated.
2. Queue storage format (journal plus SQLite, or journal only).
3. Whether a draft with any irrecoverable action requires an extra confirmation line at review time ("this will send 1 email").
