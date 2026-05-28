# Developer Style: Karpathy

*Inspired by Andrej Karpathy's published coding philosophy.*

## Code Density

- Prefer flat, linear code over deeply nested abstractions.
- No helper functions unless used in three or more call sites.
- No premature generalisation — write it twice before extracting.
- Short files over large module hierarchies.
- Inline logic is fine; readability trumps reuse.

## Abstraction

- Explicit over implicit. Write out the full thing rather than hiding it behind a named wrapper.
- Avoid layers. A function should not just call another function of the same name one level down.
- When in doubt, just inline it.

## Comments

- No comments that restate the code.
- Comment *why*, never *what*.
- A well-named variable or function removes the need for most comments.

## Error Handling

- Errors should be explicit. Avoid hiding failures behind options that silently return `None`.
- Crash early when invariants are violated; don't silently corrupt state.
- No swallowed errors.

## Tests

- Tests only for non-obvious logic.
- Unit test at the level where bugs actually hide — not at every function boundary.
- No test ceremony (no complex fixtures, no deep mocking hierarchies).

## Naming

- Names that describe what a thing *is*, not what it *does to* something.
- Avoid `Manager`, `Handler`, `Helper`, `Util`, `Wrapper` suffixes.
- Concise over verbose; single words preferred where unambiguous.

## General

- Optimise for reading speed, not writing speed.
- A new reader should be able to understand the code without asking questions.
- Remove dead code immediately. Don't comment it out.
