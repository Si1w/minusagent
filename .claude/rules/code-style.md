---
description: Code style and Rust conventions
---

# Code Philosophy: YAGNI

- Minimize abstractions — only add what's needed now.
- Keep code simple and readable.
- Avoid over-engineering and speculative features.
- Prefer deletion over addition.

# Rust Style

## Formatting

- `rustfmt` defaults. No custom overrides.
- Max line width: 100.

## Naming

- Modules: `snake_case`
- Types/Traits: `PascalCase`
- Functions/Methods: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`

## Structure

- One type per file when the type is non-trivial.
- `mod.rs` only for re-exports, no logic.
- Group imports: std → external crates → crate internals, separated by blank lines.

## File Layout Order

```rust
// 1. use statements (grouped as above)
use std::...;

use external_crate::...;

use crate::...;

// 2. Constants

// 3. Type definitions (structs, enums, type aliases)

// 4. Trait definitions

// 5. Trait implementations (impl Trait for Type)

// 6. Inherent implementations (impl Type)

// 7. Functions
```

## Error Handling

- Use `anyhow::Result` for application code.
- Use `thiserror` for library/domain errors that callers need to match on.
- No `.unwrap()` in non-test code.

## Documentation

- `///` for all pub items (structs, enums, traits, functions, methods).
- No doc comments on private items — use `//` sparingly if needed.

```rust
/// Load session history from a JSONL file
///
/// # Arguments
///
/// * `id` - Session ID or unique prefix
///
/// # Returns
///
/// The reconstructed message history. Returns empty `Vec` if file does not exist.
///
/// # Errors
///
/// Returns error if `id` matches zero or multiple sessions, or if JSONL is malformed.
pub fn load(&mut self, id: &str) -> Result<Vec<Message>> {
```

- First line: concise summary.
- `# Arguments`: list each param with `* \`name\` - description`.
- `# Returns`: describe the return value.
- `# Errors`: when returning `Result`, describe failure cases.
- `# Examples`: only when usage is non-obvious.
- Omit sections that add no value (e.g. skip `# Arguments` for zero-arg methods).

## Patterns

- Prefer `impl Trait` over `dyn Trait` when possible.
- Prefer owned types over references in public APIs unless lifetime is obvious.
- No over-commenting. Code should be self-explanatory.

# Git Conventions

- Create a new branch for each feature and submit a PR
- Commit message format:

```
<type>(<scope>): <subject>

- <body>
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`

- Review the code before committing to ensure the logic and correctness
