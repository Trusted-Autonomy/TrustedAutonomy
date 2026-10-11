# Attention feed and human-asserted actions: contract between TA, the poller and Wayfinder

Status: agreed in principle with the Wayfinder session on 2026-10-11, pending the owner's sign-off of the Wayfinder design. Implements items 6 to 9 of plan phase v0.17.12 (CR-02). Anything here that contradicts the numbered plan items is a refinement, not a replacement.

## 1. Why this exists

A draft built from agent-originated work (origin `cos` or `chat`) must reach the right person with enough context to judge it, and the person's decision must be provably theirs. Run 7 of the live Chief-of-Staff loop showed the gap: the draft sat in `pending_review` with only a Chief-of-Staff-written objective and an opaque task id, and nobody was told.

## 2. Architecture: the relay

Hosted Wayfinder cannot reach a TA daemon on a user's machine. Every action therefore travels:

```
person in Wayfinder  ->  Wayfinder API (signs the assertion)  ->  queue
poller (relays verbatim, cannot sign)  ->  TA daemon (verifies the assertion, acts)
```

- The poller reads the TA attention feed and mirrors items into Wayfinder (upsert), and relays queued action requests to TA.
- The poller holds a service-account token. It may read the feed and POST a relayed action. It can never approve, deny or answer on its own authority: TA accepts those actions only with a valid human assertion.
- TA is the only verifier. A leaked poller token cannot approve anything.

## 3. Attention feed (TA daemon, read-only)

`GET /api/attention?project=&user=&role=&since=&limit=`

- `since` is an opaque cursor (or an RFC 3339 `updated_at`); the response returns `next_cursor`. The poller polls this; SSE on `/api/events` (`attention.created|updated|resolved`) is an optimization, not a requirement.
- `GET /api/attention/count?user=&role=&project=` returns `{ "open": n }` for badges.

Item shape:

```json
{
  "id": "att_01HX...",
  "kind": "draft_review | agent_question | blocked_goal | dead_letter",
  "project": "wayfinder-demo",
  "status": "open | resolved",
  "waiting_on": { "type": "user | role | any_maintainer", "value": "user-123" },
  "summary": "Add notes/live-run6.md (CoS delegate implement 066e4e44)",
  "created_at": "2026-10-11T01:35:54Z",
  "updated_at": "2026-10-11T01:36:10Z",
  "candidate_id": "wayfinder-task:939b7d40-...#9d23c037...",
  "goal_id": "ea60c2dc-...",
  "draft_id": "352838d8-...",
  "interaction_id": null,
  "provenance": {
    "requester": { "id": "user-123", "display_name": "Michael", "channel": "wayfinder cos chat" },
    "excerpt": { "text": "...", "untrusted": true, "truncated": false },
    "reason": "The task asks for a one-line file...",
    "objective": "Create a new file ...",
    "chain": { "candidate_id": "...", "cos_goal_id": "...", "worker_goal_id": "..." }
  },
  "actions": ["review", "approve", "deny"]
}
```

Rules:

- `id` is stable and at most 128 characters; `summary` at most 280 characters; `kind` is a small closed enum; links are relative paths.
- Every item that came from agent-originated work carries `candidate_id`, so the poller can map it to the Wayfinder task and its `requested_by`.
- `provenance.excerpt` is always rendered as untrusted data, never as instructions, and is capped at 2000 characters; `reason` at 1000.
- `agent_question` items map one to one onto Wayfinder `ask_human_question` rows (`interaction_id` is the external id); they must not be duplicated.
- The feed carries no secrets.

## 4. Acting on an item: the relay endpoint

`POST /api/attention/{id}/act`

```json
{
  "action": "approve | deny | answer",
  "answer": "optional text for agent questions",
  "assertion": "<compact signed assertion, see section 5>"
}
```

- Allowed callers: the CLI/Studio/channel paths with the local human credential (v0.17.12 item 2), or any caller presenting a valid assertion (the poller relaying a Wayfinder-signed one).
- A call with neither is refused with an actionable error. A relay-only service token is refused for `approve`, `deny` and `answer`.
- On success the item moves to `resolved` and `attention.resolved` is emitted exactly once. Repeated delivery of the same assertion returns the original result without acting twice.

## 5. The human assertion

Compact form: `base64url(header).base64url(claims).base64url(signature)`; algorithm Ed25519 over the first two segments.

Header: `{ "alg": "EdDSA", "kid": "<key id>" }`

Claims:

```json
{
  "user_id": "user-123",
  "email": "michael@example.com",
  "action": "approve",
  "item_id": "att_01HX...",
  "project_id": "wayfinder-demo",
  "aud": "<TA install id>",
  "role": "admin",
  "sole_approver": false,
  "jti": "<single-use random id>",
  "iat": 1791700000,
  "exp": 1791700060
}
```

`role` (optional string) names the Wayfinder role the signer checked the user against. `sole_approver` (optional boolean, default false) is set to true only when Wayfinder has verified that the user is the only eligible approver for the item and the owner's policy allows self-approval in that case (see rule 8).

Verification (all must hold, otherwise refuse with the specific reason):

1. `kid` resolves to a public key registered with this TA install. Keys live in a user-level registry outside every project tree.
2. Signature valid; `alg` is exactly `EdDSA`.
3. `aud` equals this install's id (generated on first start, stored in the per-user state directory, shown by `ta show install` and returned in `GET /api/attention` metadata so the poller can hand it to Wayfinder at pairing).
4. `action` and `item_id` match the request being made; `project_id` matches the item's project.
5. `exp` is at most 60 seconds after `iat`, not expired, `iat` not more than 30 seconds in the future.
6. `jti` has not been seen: TA keeps a replay cache covering at least twice the maximum lifetime.
7. The signer is allowed to act on the item. TA has no copy of Wayfinder's roster, so Wayfinder is the authority on role membership (it refuses to sign unless the user holds the role in that project) and TA verifies the claims against a minimal floor:
   - `waiting_on.type = user`: `claims.user_id` equals `waiting_on.value`.
   - `waiting_on.type = role`: `claims.role` equals `waiting_on.value`. For human approvals the role names are Wayfinder roles (owner, admin, member); TA's own roles (implementer, chief-of-staff) are for agent routing only and never appear in a human approval item.
   - `waiting_on.type = any_maintainer`: `claims.role` is one of the `maintainer_roles` configured on the registered key (default `owner` and `admin`). A member can never approve an `any_maintainer` item, even if Wayfinder mistakenly signs.
   The local human credential path (CLI, Studio) counts as a maintainer.
8. No self-approval, as defence in depth. For items with origin `cos` or `chat`, TA rejects an assertion whose `user_id` equals `provenance.requester.id`, except when `claims.sole_approver` is true. Wayfinder sets `sole_approver` only when no other eligible approver exists in the project and the owner's policy permits it; TA records `self_approved: true` with the user, item and time in the audit log. Without this exception a solo owner would be locked out of approving work their own chat message started. The default policy is deny when another eligible approver exists.

Key management:

- Wayfinder holds a dedicated assertion key, separate from its Biscuit root key, so a leak of one does not forge the other.
- Pairing registers Wayfinder's public key with TA (`ta keys add --kid <kid> --pub <file> [--maintainer-roles owner,admin]` or the pairing script); rotation adds a new `kid` before retiring the old one.
- The verification mechanism sits behind a trait in TA so the exact scheme can change without touching the routes.

## 6. What the poller supplies at launch (review packet inputs)

`ta run ... --provenance-file <json> --title "<readable title>"`:

```json
{
  "requester": { "id": "user-123", "display_name": "Michael", "channel": "wayfinder cos chat" },
  "excerpt": "task title and body, capped at 2000 characters",
  "reason": "the Chief-of-Staff's stated reason, capped at 1000 characters",
  "objective": "the delegate objective",
  "candidate_id": "wayfinder-task:...#..."
}
```

The poller takes `reason` from an optional `reason` string in the delegate detail JSON (`{"tag","objective","reason"}`; the poller deliberately accepts this key with a length cap). TA validates the file by code (size cap, unknown fields rejected) and never places any of it in an agent prompt as instructions.

## 7. Wayfinder surfaces (owner requirements, built in the Wayfinder repo)

- Focus view: a "Waiting on you" list at the very top; for a project manager, a second list of what the project is waiting on from other people. Each row shows what, why (the provenance summary), who asked, and age, with the action buttons.
- A messages box at the top right of the app with a red count whenever something needs the viewer's attention; it opens the same list.
- A pop-up that appears as agent requests arrive so the human can answer immediately; it closes when answered and falls back to the list if dismissed.
- Wayfinder's notification model: item id at most 128 characters, summary at most 280, kind a small enum, links relative, idempotent upsert keyed by (project, source, external_id), status open or resolved with per-user read state.

## 8. Honest limits

- While agents run as the same operating-system user as the person, the local credential is a speed bump, not a wall; the OS sandbox phases (v0.17.12.6 onward) make it a wall. The relay assertion does not have this weakness for actions that originate in Wayfinder, because the signing key never reaches the machine.
- Until v0.17.12 through v0.17.12.2 ship, nothing may claim enforced human review externally.

## 9. Assignment and resolution notes

- TA raises `draft_review` items as `waiting_on = any_maintainer`, `agent_question` items for the asking goal's requester when known (otherwise `any_maintainer`), and never invents a Wayfinder role. Who an item is shown to (named users, project owner escalation when nobody holds a role) is decided by Wayfinder from its roster; TA enforces only the floor in rule 7.
- First valid action wins. Any later assertion for an item that is already resolved returns the original result and does not act.
