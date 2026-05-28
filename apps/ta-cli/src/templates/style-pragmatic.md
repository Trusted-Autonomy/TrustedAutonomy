# Developer Style: Pragmatic

Balanced defaults: WHY-only comments, extract when used 3+ times, anyhow for errors.

## Comments

- Comment the *why*, not the *what*.
- Delete comments that describe what the code already says clearly.
- One-line comments acceptable for non-obvious decisions or invariants.

## Helper Functions

- Extract a helper when it appears three or more times.
- Name helpers after what they *return* or *represent*, not what they *do*.
- Keep helpers close to their callers.

## Error Handling

- `anyhow` for binaries and applications; typed errors for library crates.
- Add context with `.context("what we were trying to do")`.
- Log errors at the point of origin; propagate, don't swallow.

## Tests

- Unit tests for pure logic, edge cases, and known past bugs.
- Integration tests for user-visible workflows.
- Skip testing trivial delegation.

## Abstraction

- Extract an abstraction when there are three or more similar patterns.
- Avoid over-engineering for a single use case.
- Prefer concrete types over trait objects unless multiple implementations are expected.

## Module Organization

- One concept per module.
- Keep modules small enough to read in one sitting.
- Avoid deeply nested module paths.

## Dependencies

- Well-maintained external crates preferred over re-implementing complex logic.
- Check that a dependency is actively maintained before adding it.
