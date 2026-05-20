# Configure Your Workflow

TA needs a `workflow.toml` to understand your project. Create one in your project root.

## Python project

```toml
[project]
name = "my-python-app"
language = "python"
agent = "claude-code"

[verify]
command = "python -m pytest"
```

## TypeScript / Node.js project

```toml
[project]
name = "my-ts-app"
language = "typescript"
agent = "claude-code"

[verify]
command = "npm test"
```

## Rust project

```toml
[project]
name = "my-rust-app"
language = "rust"
agent = "claude-code"

[verify]
command = "cargo test --workspace"
```

The agent uses `[verify]` to check its work before submitting a draft. If verification fails, the draft includes warnings so you can see what the agent attempted.
