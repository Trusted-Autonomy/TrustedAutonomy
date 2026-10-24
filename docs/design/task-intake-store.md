# Task Intake Store: On-Disk Format

Reference for the store behind `ta_request_task` (crate `ta-task-intake`, not to be confused with the `ta-intake` trigger layer). It is written for whoever builds a drain consumer on top of it, such as the virtual-team project-manager poller. The Rust types are documented in the crate; this page is the file-level contract.

A goal agent asks for a Wayfinder task or a KPI datapoint. The gateway queues the request. A consumer drains the queue in batches, turns requests into a draft a person reviews, and reports outcomes back. **Nothing in this store executes a request.**

## Directory

One store per project root, at `<project>/.ta/task-intake/`. The directory is mode `0700` and every file is `0600`.

| File | Written by | Content |
|---|---|---|
| `queue.jsonl` | gateway (append), drain (rename away) | `IntakeRecord` lines: the request bodies waiting for a drain. |
| `ledger.jsonl` | gateway, drain, consumer, recovery | Append-only `LedgerEvent` lines. **The only place request state lives.** |
| `batch-<id>.jsonl` | drain | A drained `queue.jsonl`, renamed atomically. Immutable. `<id>` is `<utc-timestamp>-<8 hex>`. |
| `.lock` | everyone who writes | `flock(2)` target. One exclusive lock serialises append, drain, acknowledge and recovery. Readers never take it. |

Every line is one JSON object, at most 4096 bytes. A request body (typed fields plus `why_now`) is at most 2048 bytes; the gateway refuses more.

## Queue record (`IntakeRecord`)

```json
{"schema_version":1,"request_id":"req-3f9c…","project_id":"proj_abc","goal_id":"<uuid>",
 "role":"builder","body":{"kind":"create-task","title":"…","verb":"implement","description":"…"},
 "why_now":"…","blocks_goal":true,"target":"…","dedupe_key":"<sha256 hex>","created_at":"2026-…Z"}
```

`body.kind` is `create-task` (`title`, `verb`, optional `description`) or `record-datapoint` (`metric_id`, `value`, `source`). `goal_id`, `role` and `project_id` come from the gateway, never from the agent. `dedupe_key` is a SHA-256 over project, kind, target and the whitespace- and case-normalised body, so one request cannot be queued twice while it is live.

**Every text field is untrusted agent output.** The records `drain` returns are raw. A consumer must fence them (`ta_task_intake::fence`, which uses `ta_goal::provenance::fence_untrusted`) before any model or person reads them.

## Ledger events (`LedgerEvent`)

`transition` events carry `request_id`, `project_id`, `state`, `at`, `actor`, and where relevant `batch_id`, `lease_expires_at`, `draft_id`, `reason`. The first event of a request, `queued`, also carries `origin` (goal, role, kind, dedupe key, creation time). A `duplicate` event records another goal asking for something already live, so that goal can read the status too.

| State | Meaning | Next states |
|---|---|---|
| `queued` | Waiting in `queue.jsonl`. | `claimed`, `expired` |
| `claimed` | Drained into a batch; a consumer holds a lease. | `queued` (lease lapsed), `drafted`, `rejected`, `expired` |
| `drafted` | A draft containing it awaits review. | `applied`, `rejected`, `expired` |
| `applied` | Wayfinder confirmed the change. Terminal. | none |
| `rejected` | Declined, with a reason. Terminal. | none |
| `expired` | Gave up (too old, or claim attempts used up). Terminal. | none |

The requester sees only `queued` (anything not terminal) or `done` (any terminal state).

## Write rules

* Every append is a single `write_all` of `line + "\n"` followed by `fsync`, under the exclusive lock. Concurrent writers, threads or processes, never interleave bytes.
* Enqueue writes the queue line first and the ledger `queued` event second.
* If a writer dies mid-line the file ends with a torn line. Readers skip it and report a warning. The next append starts with a newline if the file does not end with one, so a fragment can never corrupt a new record.

## Drain and recovery

`drain` runs recovery, renames `queue.jsonl` to `batch-<id>.jsonl` while holding the lock, writes a `claimed` event with a lease (default 30 minutes) for each request, and puts any requests beyond the batch limit (default 25) back into a fresh queue.

Recovery is deterministic from the files and the clock, so a process killed anywhere between the rename and the acknowledgement is resolved the same way every time:

* A request `claimed` past its lease goes back to `queued` and its record is copied from the batch into the queue. After `max_claim_attempts` (default 3) it becomes `expired` instead.
* A request that is `queued` in the ledger but found only in a batch file is copied back into the queue.
* A queue record with no ledger event (a crash between the two enqueue writes) is treated as `queued` and gets its missing event.
* A request `queued` longer than the TTL (default 14 days) becomes `expired`.

A consumer acknowledges with `drafted`, `applied`, `rejected` or `expired`. All ids in an acknowledgement are validated against the state machine before any event is written.

## Admission

Before appending, the store applies, in order: dedupe against live requests in the same project (returns the existing `request_id`, charges nothing), then the caps. The caps count per goal (all requests ever), per role per rolling hour, and per project outstanding. Defaults and the `[task_intake]` keys that change them are in `docs/USAGE.md`. A refused request reports `queue full, retry or ask_human` and names the cap and its config key.

## Audit

Every transition, duplicate and refusal also goes to the project's `.ta/audit.jsonl` as a `task_intake` event whose metadata carries `event`, `project_id`, `request_id`, `goal_id` and, where applicable, `draft_id`. If the audit write fails the error is logged and the recorded state is kept.

## Not in this store

Reading batches into drafts, project-manager handling, and Wayfinder idempotency gating for datapoints belong to other phases. A `record-datapoint` request is accepted into the queue like any other; whether it may be applied is decided by the consumer and Wayfinder.

The ledger is not compacted. It grows by one line per transition.
