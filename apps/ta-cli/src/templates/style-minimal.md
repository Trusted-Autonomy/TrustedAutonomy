# Developer Style: Minimal

Bare-bones engineering. Every line must justify its existence.

## Comments

- No comments. Code should be self-documenting.
- Exception: non-obvious invariants or workarounds for specific bugs.

## Helper Functions

- Only extract a helper if it is called three or more times.
- Inline single-use logic.

## Error Handling

- Explicit error types; no `Box<dyn Error>` or string errors in library code.
- Propagate with `?`; no `unwrap` except in tests.

## Tests

- Test only non-obvious logic.
- No tests for trivial getters, delegation, or pure wrappers.

## Abstraction

- No layers that add no logic.
- No interfaces with a single implementation.
- No registry patterns unless the number of variants is genuinely open.

## Dependencies

- Prefer the standard library over external crates for simple tasks.
- Each new dependency must earn its place.
