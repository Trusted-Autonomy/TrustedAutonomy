# Developer Style: Documented

Full docstrings, typed interfaces, and integration tests preferred.

## Comments and Documentation

- Every public function, type, and module has a docstring.
- Docstrings describe: what the function does, its parameters, return value, and error cases.
- Non-obvious side effects are documented.
- Examples in docstrings for complex APIs.

## Types and Interfaces

- Typed interfaces for all module boundaries. No stringly-typed APIs.
- New vocabulary types over primitive obsession.
- Explicit return types; no inferred return types for public functions.

## Error Handling

- Typed error enums for library crates.
- Error variants describe every distinct failure mode.
- Errors include context (which file, which step, which ID).

## Tests

- Integration tests preferred over unit tests for core flows.
- Unit tests for complex algorithms and edge cases.
- Test the public API, not internal implementation.
- Tests are self-contained — no shared mutable state between tests.

## Abstraction

- Trait/interface per concern.
- Prefer composition over inheritance.
- Separate data from behaviour; avoid god objects.

## Naming

- Verbose names are fine: `parse_configuration_from_path` over `parse_cfg`.
- Match domain terminology exactly.
- Consistent naming patterns across the codebase.
