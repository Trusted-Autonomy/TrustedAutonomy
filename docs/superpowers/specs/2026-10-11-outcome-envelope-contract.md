# Outcome Envelope Contract (version 1)

**Status:** shipped in TA v0.17.12.2 (red-team findings CR-04, CR-32).
**Audience:** whoever implements verification in the `ta-virtual-team` poller. This document is meant to be sufficient on its own.
**Reference implementation:** `crates/ta-agent-whiteboard/src/outcome_envelope.rs` (the test `contract_test_vector_verifies_and_reproduces` pins the vector below).

## 1. What this gives you

The daemon authenticates which role posted an outcome, stamps that into the message and signs it with an Ed25519 key only the daemon holds. A verifier that has the public key can prove an outcome came from the Chief-of-Staff launch and not from another role, and can refuse forgeries, tampering, replays and stale messages. The NATS bus is untrusted by design: a direct publish fails verification even when the bus is reachable.

A signature proves only that the daemon authenticated the caller and the caller held the `whiteboard:outcome:send` right (granted only to roles in `[whiteboard] outcome_roles`, default `chief-of-staff`). Still check `stamp.role` yourself.

## 2. Compatibility and migration

The change is additive and versioned.

- The four original fields (`candidate_id`, `outcome`, `detail`, `new_task_title`) keep their names and meanings.
- New top-level fields: `v`, `stamp`, `sig`. A poller that ignores them keeps working (the current `Outcome` struct has no `deny_unknown_fields`).
- The daemon stamps **every** outcome from this release on.
- Do not use top-level `tag` or `objective`; the poller already rejects those.

Migration order:

1. Ship TA v0.17.12.2. Envelopes are now stamped. Nothing changes for an unmodified poller.
2. Implement verification from this document. Run it log-only first, then enforcing.
3. Set `[whiteboard] require_stamped_outcomes = true` in `.ta/workflow.toml` (it is on by default under the `strict` posture, off in the default posture). With it on, the daemon check endpoint (section 9) refuses unstamped legacy envelopes. Your poller should do the same: once enforcing, treat an envelope without `v`/`stamp`/`sig` as forged.
4. Lock down NATS (credentials, loopback) so only the daemon can publish. Verification makes this defense in depth rather than the only barrier.

A Chief-of-Staff launch started before the upgrade holds a role-less token and cannot post outcomes ("outcome refused ... no role"). Relaunch it through the daemon (wake-on-demand launches mint a fresh token every time, so this resolves itself on the next intake).

Workers cannot post outcomes at all (they report through drafts). If you relied on a worker calling `ta_whiteboard_outcome_send`, that now fails with a 403 that names the role.

## 3. Wire format

One JSON object per stream message on stream `external-outcome` (NATS subject `wb.handoff.external-outcome`), UTF-8, at most 65,536 bytes.

```json
{"candidate_id":"wayfinder-task:t1#abc","outcome":"reply","detail":"hello","new_task_title":null,"v":1,"stamp":{"team_session":"sess-1","role":"chief-of-staff","goal_id":"11111111-2222-3333-4444-555555555555","target":"local","msg_id":"00000000-0000-4000-8000-000000000001","issued_at":1800000000},"sig":{"kid":"k20261011-0a0b0c0d","alg":"ed25519","value":"IjA7ZDT1qGUSpXNd5N1ZqWphJIcxkEt9ERr8-kA9qz2mf3ZHlM5Yi1WxM2jYqt0wzi4iiFqCQiXlLgKZjoO0Bw"}}
```

A stamped envelope has exactly these top-level keys: `candidate_id`, `outcome`, `detail`, `new_task_title` (may be `null` or absent), `v`, `stamp`, `sig`. **Any other top-level key makes it invalid** (this blocks smuggling `tag`/`objective` onto a genuine envelope). `stamp` and `sig` also reject unknown keys. Having some but not all of `v`, `stamp`, `sig` is invalid, never "legacy".

### Fields and limits

| Field | Type | Limit | Rules |
|---|---|---|---|
| `candidate_id` | string | 1..=256 bytes | no control characters |
| `outcome` | string | 1..=32 bytes | exactly one of `done`, `blocked`, `new_work`, `reply`, `delegate` |
| `detail` | string | 0..=16,384 bytes | free text; for `delegate`, JSON text `{"tag":..,"objective":..}` |
| `new_task_title` | string or null | 0..=512 bytes | no control characters; only meaningful for `new_work` |
| `v` | integer | | must be `1` |
| `stamp.team_session` | string | 1..=128 bytes | `[A-Za-z0-9._:-]` |
| `stamp.role` | string | 1..=64 bytes | same charset; the authenticated role |
| `stamp.goal_id` | string | 1..=64 bytes | same charset; the role's goal id, or `unresolved` when the daemon could not attribute a goal |
| `stamp.target` | string | 1..=128 bytes | same charset; execution target identity, `local` unless the sender ran on a remote runner (for example `runner:build-7`) |
| `stamp.msg_id` | string | 1..=64 bytes | same charset; unique per envelope (a UUID v4) |
| `stamp.issued_at` | integer | > 0 | unix seconds, daemon clock |
| `sig.kid` | string | 1..=64 bytes | same charset; key id in the registry |
| `sig.alg` | string | | must be `ed25519` |
| `sig.value` | string | 86 chars | base64url, no padding, of the 64-byte signature |

## 4. Canonical signing input

The signature is over these exact bytes (UTF-8, every line ends with a single `\n`, no other whitespace):

```
ta-outcome-envelope/v1
kid:<sig.kid>
team_session:<stamp.team_session>
role:<stamp.role>
goal_id:<stamp.goal_id>
target:<stamp.target>
msg_id:<stamp.msg_id>
issued_at:<stamp.issued_at as a decimal integer>
body_sha256:<64 lowercase hex chars>
```

`body_sha256` is the SHA-256 of the concatenation, in this order, of `candidate_id`, `outcome`, `detail`, `new_task_title`, where each present value is encoded as an 8-byte **big-endian** length followed by its UTF-8 bytes. An absent or `null` `new_task_title` is encoded as the 8 bytes `ff ff ff ff ff ff ff ff` and nothing else (distinct from the empty string, which is eight zero bytes).

JSON key order, whitespace and escaping on the wire do not matter; you sign and verify over the decoded values, so no JSON canonicalization is needed. Because the stamp charset excludes newlines and `:` only appears after the field name, the input is unambiguous.

### Known-answer vector

Private seed: 32 bytes of `0x07`. Public key (base64url): `6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw`. Key id `k20261011-0a0b0c0d`. The envelope in section 3 has `body_sha256` = `676e7fc106d2ade491bd99c08a8bff3acd183891c64950e67a5aa1af86202530`, so the signing input is:

```
ta-outcome-envelope/v1
kid:k20261011-0a0b0c0d
team_session:sess-1
role:chief-of-staff
goal_id:11111111-2222-3333-4444-555555555555
target:local
msg_id:00000000-0000-4000-8000-000000000001
issued_at:1800000000
body_sha256:676e7fc106d2ade491bd99c08a8bff3acd183891c64950e67a5aa1af86202530
```

Ed25519 signatures are deterministic, so your verification of the envelope in section 3 must succeed and re-signing must reproduce `sig.value` exactly.

## 5. Public key distribution

The verification key is public. The private key never leaves the daemon.

- Registry file: `<TA state directory>/projects/<project id>/outcome-keys/public-keys.json`. Print the exact path and fingerprints with `ta show outcome-key` (run it in the project). The state directory is per user and outside the project tree; agents cannot reach it once the sandbox work lands, and today the file holds only public keys, so reading it gives an agent nothing it can sign with. The poller runs as the same user and reads it directly.
- The daemon writes it atomically (write then rename), so a concurrent read is never torn. Re-read it when you see an unknown `kid`, and at least on a timer (suggested: every 60 seconds).
- Do not copy the key into a project file or a config the agents can write. Pin the **fingerprint** (`sha256:` plus the first 16 bytes of SHA-256 over the raw 32-byte public key, hex) that `ta show outcome-key` prints if you want to detect registry tampering.

Registry format:

```json
{
  "v": 1,
  "keys": [
    {"kid":"k20261011-0a0b0c0d","alg":"ed25519","public_key":"<base64url 32 bytes>","status":"active","created_at":1800000000},
    {"kid":"k20260901-11223344","alg":"ed25519","public_key":"...","status":"retired","created_at":1790000000,"retired_at":1800000000}
  ]
}
```

`status` is one of `pending`, `active`, `retired`, `revoked`. Unknown extra fields must be ignored.

## 6. Key rotation and revocation

Rotation adds the new key id before retiring the old one, so there is never a moment when a valid signature carries a key you have not seen.

1. `ta outcome-key rotate`: publishes a new key as `pending`. The old key still signs. Verifiers load the registry and should accept `pending` keys.
2. Wait for pollers to reload (at least one reload interval).
3. `ta outcome-key promote`: the pending key becomes `active` and signs from now on; the previous key becomes `retired` with `retired_at`.

Verifier rules per key status:

- `active`, `pending`: verify.
- `retired`: verify only if `stamp.issued_at <= retired_at + 30` (allowing clock skew). An envelope issued after the key retired is invalid.
- `revoked`: never verify. Revocation is for a suspected leak: `ta outcome-key revoke <kid>`. Revoking the active key immediately installs a replacement so the daemon keeps stamping, and deletes the revoked private key. Envelopes already signed by it stop verifying, which is intended.
- Unknown `kid`: reload the registry once; if still unknown, invalid.

Every rotation, promotion and revocation is written to the TA audit log (`outcome_key_changed`, key id only).

## 7. Verification checklist (pseudocode)

```
fn verify(bytes, ring, policy, seen, now) -> Verified | Legacy | Error:
  1. if len(bytes) > 65536                      -> reject (too large)
  2. obj = parse JSON object                    -> reject if not an object
  3. present = which of {v, stamp, sig} exist
     if none present:
         if policy.require_stamped             -> reject (unstamped)
         else                                  -> Legacy (carries NO proof of sender)
     if some but not all present               -> reject (downgrade attempt)
  4. if obj.v != 1                             -> reject (unsupported version)
  5. parse strictly: reject any unknown top-level key, and unknown keys in stamp/sig
  6. validate every field against the table in section 3 (charset, limits, outcome enum)
  7. if sig.alg != "ed25519"                   -> reject
  8. if ring is empty                          -> reject (cannot verify: load the registry)
  9. key = ring[sig.kid]; unknown              -> reload once, else reject
 10. if key.status == revoked                  -> reject
     if key.status == retired and stamp.issued_at > key.retired_at + 30 -> reject
 11. input = canonical signing input (section 4)
     if !ed25519_verify(key.public_key, input, base64url_decode(sig.value)) -> reject
 12. age = now - stamp.issued_at
     if age > policy.max_age (default 300 s)   -> reject (stale)
     if -age > 30                              -> reject (from the future)
 13. if policy.allowed_roles non-empty and stamp.role not in it -> reject
     (the poller should require role == "chief-of-staff" for reply and delegate)
 14. if stamp.msg_id in seen                   -> reject (replay)
     seen.insert(stamp.msg_id, issued_at)      // only after every check above passed
     (entries older than max_age + 30 s can be dropped; step 12 refuses them anyway)
 15. return Verified{ body, stamp, kid }
```

Also (from CR-32): touch the in-flight candidate only after a verified outcome is acted on, and keep binding `candidate_id` to the per-dispatch nonce you already use. The signature does not replace those checks, it makes them trustworthy.

## 8. Example verification in Rust

Dependencies: `ed25519-dalek = "2"` (default features off, `std`), `sha2 = "0.10"`, `base64 = "0.22"`, `serde_json`.

```rust
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use serde_json::Value;

fn body_sha256(c: &str, o: &str, d: &str, t: Option<&str>) -> String {
    let mut h = Sha256::new();
    for part in [c, o, d] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    match t {
        Some(t) => { h.update((t.len() as u64).to_be_bytes()); h.update(t.as_bytes()); }
        None => h.update(u64::MAX.to_be_bytes()),
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// `public_key_b64`: the registry entry for `kid`. Returns Ok(()) if the
/// signature verifies. Do the field validation, key-status, staleness, role
/// and replay checks from section 7 around this.
fn verify_signature(env: &Value, public_key_b64: &str) -> Result<(), String> {
    let s = |v: &Value, k: &str| v[k].as_str().map(str::to_string).ok_or(format!("missing {k}"));
    let (stamp, sig) = (&env["stamp"], &env["sig"]);
    let input = format!(
        "ta-outcome-envelope/v1\nkid:{}\nteam_session:{}\nrole:{}\ngoal_id:{}\ntarget:{}\nmsg_id:{}\nissued_at:{}\nbody_sha256:{}\n",
        s(sig, "kid")?, s(stamp, "team_session")?, s(stamp, "role")?, s(stamp, "goal_id")?,
        s(stamp, "target")?, s(stamp, "msg_id")?,
        stamp["issued_at"].as_i64().ok_or("missing issued_at")?,
        body_sha256(
            &s(env, "candidate_id")?, &s(env, "outcome")?, &s(env, "detail")?,
            env["new_task_title"].as_str(),
        ),
    );
    let pk: [u8; 32] = B64.decode(public_key_b64).map_err(|e| e.to_string())?
        .try_into().map_err(|_| "public key is not 32 bytes")?;
    let sig_bytes: [u8; 64] = B64.decode(s(sig, "value")?).map_err(|e| e.to_string())?
        .try_into().map_err(|_| "signature is not 64 bytes")?;
    VerifyingKey::from_bytes(&pk).map_err(|e| e.to_string())?
        .verify(input.as_bytes(), &Signature::from_bytes(&sig_bytes))
        .map_err(|_| "signature does not verify (forged or tampered)".to_string())
}
```

TA's own `ta_agent_whiteboard::outcome_envelope::verify_payload` implements the whole checklist and can be depended on directly instead (workspace crate `ta-agent-whiteboard`).

## 9. Daemon-side check endpoint

Until the poller verifies locally, it can ask the daemon: `POST /api/whiteboard/outcome/check` with `{"payload": "<exact stream payload>", "consume": true|false}`. The daemon applies its own registry and `[whiteboard]` settings (`require_stamped_outcomes`, `outcome_max_age_secs`, `outcome_roles`). 200 means accepted (`"verified": true` for a stamped envelope, `"legacy": true` with a note for an unstamped one when the setting is off); 422 carries the refusal reason in `error`. With `consume` false it is a dry run; with `consume` true a verified `msg_id` is remembered and a second presentation is refused. This endpoint does not replace local verification: a poller that trusts the bus for transport still needs the daemon to be the judge.

## 10. Settings

In `.ta/workflow.toml`:

```toml
[whiteboard]
require_stamped_outcomes = true   # default: off, on under the strict posture
outcome_roles = ["chief-of-staff"]  # roles granted whiteboard:outcome:send at launch
outcome_max_age_secs = 300         # staleness window used by the daemon check
```
