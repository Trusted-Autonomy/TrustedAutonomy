# General-Purpose License Server — Port + New Features (Design + Implementation Plan)

**Status**: draft, 2026-08-16. Not yet reviewed or approved for implementation.

## Why this exists

`cinepipe-license` (`~/development/amplifiedxai/cinepipe-license`) is a mature Python/FastAPI/SQLAlchemy license, seat, and credit-metering server, built for CinePipe. A second, unrelated product (OnPace, formerly `agentic-pm`, a Rust project) now needs the same kind of thing: license validity checks plus credit metering, but for its own tiers and its own "job" vocabulary. Rather than build a second license server from scratch, generalize the existing one and let both products consume it, the same precedent as `task-graph` being extracted out of TA-adjacent work into a standalone, independently-versioned dependency.

**Deliberate choice, per direct instruction: this is a port of the existing Python codebase, not a rewrite to Rust.** The generic server stays Python/FastAPI/SQLAlchemy.

## A real deviation from this repo's convention, flagged explicitly

This workspace is disciplined, pure-Rust (`crates/ta-*`, `./dev cargo build/test/clippy/fmt --workspace` as the verification gate — see root `CLAUDE.md`). There is no precedent here for a full backend service in another language; the one non-Rust artifact in the repo is a VS Code extension (`plugins/vscode-ta`), a different category (IDE plugin, not a service). Dropping a Python FastAPI app into `crates/` would violate that convention outright: it wouldn't build under `cargo build --workspace`, and `ta-*` crate naming doesn't fit a service that isn't a crate.

**Recommendation**: a new top-level directory, `license-server/` (sibling to `crates/`, `apps/`, `agents/`), explicitly carved out of the Rust verification loop with its own CI workflow (`.github/workflows/license-server-ci.yml`, running `pytest`/`ruff`, not `cargo`). Document this exception in root `CLAUDE.md` when implementation starts, so it isn't mistaken for scope creep or an accident later.

## Generic vs. CinePipe-specific split

Everything in `models.py` is already close to generic except one concept:

| Stays generic (moves as-is) | Stays CinePipe-specific (extracted out) |
|---|---|
| `License`, `Account`, `User`, `AccountMembership`, `AuthIdentity`, `Subscription`, `Seat`, `Validation`, `TelemetryEvent`, `WebhookEvent` | `Universe` (CinePipe's own production/world concept) |
| `CreditLedgerEntry`, `CreditHold`, `Job`, `Budget`, `BudgetAlertLog` — these are already keyed by an opaque `job_type` string and a `project_id`/`budget_key` (ltree), not by anything CinePipe-shaped | The `budget_key` **ltree hierarchy's semantics** (what the path segments mean) — the ltree *mechanism* is generic and stays; only the meaning CinePipe assigns to its levels (universe → project) is product-specific and configured, not hardcoded |
| `payments/` (Stripe, Paddle, devpayments), `crypto.py`, `keygen.py`, `budgets.py`, `credits.py`, `reconciler.py`, `rate_limit.py` | — |

`Project` stays generic too (it's already just "a thing under an account that budget/credits attach to" — CinePipe happens to nest it under `Universe`, OnPace would nest its own `project_id` directly under `Account` with no universe layer at all). The only code change this split requires: make `Universe` optional/pluggable rather than a required FK hop, so a consumer with no universe concept (OnPace) can create a `Project` directly under an `Account`.

## Three new features (build into the generalized server, not bolted onto cinepipe-license first)

1. **Universal permanent license purchase path.** Schema already supports it (`License.expires_at = NULL` + `product = NULL`). Missing: a one-time-purchase flow. Add a one-time Stripe Checkout price (and Paddle equivalent) and a webhook handler variant that mints a `License` row directly instead of a `Subscription` row — the existing `payments/stripe_provider.py`/`payments/paddle.py` webhook dispatch is the extension point.
2. **Server-side tier enforcement on credit-hold creation.** Today `credits.py`'s hold-creation path doesn't check the caller's `Subscription.tier` at all — license validity and credit consumption are separate, but nothing stops a "PM-only" tier account from opening a credit hold. Add a tier/feature check (a `Subscription.tier` value or a `features` JSONB flag on `License`) that the credit-hold endpoint enforces server-side, not left to client discipline.
3. **Hosting-cost passthrough via metered billing.** New: report a periodic usage number (e.g. per-project or per-shard hosting cost) to Stripe/Paddle's metered-billing API so it becomes a real invoice line item, charged as money, not debited as credits. Uses the same `payments/` integration seam as #1.

## New consumer: OnPace needs a Rust client

`cinepipe-license` already ships a Python `client/` SDK. OnPace is Rust, so this port creates a real new dependency: a `ta-license-client` crate (thin HTTP client: validate license, open/commit/release credit holds, read balance), published the same way `task-graph` is — a versioned git dependency OnPace's `Cargo.toml` points at. Scope it in the same implementation pass as the server port; a server with no working client isn't actually usable yet.

## Build order

1. Extract `Universe` out of the generic kernel; make `Project` attachable directly to `Account` (no forced universe hop). Verify against cinepipe-license's *existing* test suite run against the extracted code, zero behavior change for CinePipe's own usage.
2. Stand up `license-server/` in this repo with its own CI lane; port the now-generic code there.
3. Build the three new features (universal license purchase, server-side tier enforcement, hosting-cost passthrough) directly in the ported server, not back in `cinepipe-license`.
4. Build `ta-license-client` (Rust) and wire it into OnPace for real license validation (replacing nothing yet — OnPace has no license gating today) and, once OnPace's virtual-team dispatch exists, credit-hold calls for agent-execution jobs only, never for PM CRUD.
5. Only after 1–4 are working and tested: `cinepipe-license` itself gets rebased onto this server (separate plan, `~/development/amplifiedxai/cinepipe-license/docs/superpowers/specs/2026-08-16-rebase-onto-ta-license-server-design.md`).

## Explicitly not decided here

Exact versioning/release story for `license-server/` and `ta-license-client` (does this repo's existing `v<semver>` tag-triggered release workflow extend to a Python service, or does it need its own?). Needs a decision before step 2, not blocking the design itself.
