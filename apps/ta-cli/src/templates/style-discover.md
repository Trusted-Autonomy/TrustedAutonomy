# Style Discovery: Codebase Analysis Prompt

You are analysing a codebase to infer the coding style conventions actually in use.
Your goal is to produce a `style.md` file that captures observed patterns with confidence indicators.

## What to examine

**Comment density and style**
- Ratio of comment lines to code lines (sample 10–20 files)
- Are comments explaining *why* or *what*?
- Do public functions have docstrings?

**Function length distribution**
- What is the typical function length? Short (under 20 lines), medium (20–80), or long (80+)?
- Are there many small helper functions or fewer larger ones?

**Abstraction patterns**
- Number of trait/interface definitions vs concrete types
- How many helper modules vs inline logic?
- Depth of module nesting

**Error handling approach**
- Is `anyhow`/`thiserror` used? Typed enums? String errors? Panics?
- Are errors propagated with `?` or matched inline?

**Test structure**
- Are tests in the same file (unit tests) or separate integration test files?
- What is the test-to-code ratio?
- Are tests heavily mocked or do they use real data?

**Naming conventions**
- Function naming: `verb_noun` vs `get_X` vs noun-only?
- Type naming: verbose or abbreviated?
- Are there common suffixes like `Manager`, `Handler`, `Service`?

## Output format

Produce a Markdown file with this structure:

```markdown
# Developer Style: [Project Name]

*Inferred from codebase analysis. Review and edit before using.*

## Code Density
[Observed pattern with confidence: high/medium/low]

## Error Handling
[Observed pattern with confidence: high/medium/low]

## Abstraction
[Observed pattern with confidence: high/medium/low]

## Tests
[Observed pattern with confidence: high/medium/low]

## Naming
[Observed pattern with confidence: high/medium/low]

## Comments
[Observed pattern with confidence: high/medium/low]
```

Include a confidence indicator (high/medium/low) for each section based on how consistently the pattern appears. Mark sections "low confidence" if you saw contradictory examples.

## Important

- Base everything on *what is actually in the code*, not what you think should be there.
- If a pattern is absent (e.g., no comments at all), note the absence explicitly.
- Do NOT invent rules that aren't supported by the code.
- The output will be reviewed by the developer before being saved — make it honest, not aspirational.
