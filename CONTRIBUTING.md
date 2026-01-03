# Contributing to mik

Thank you for your interest in contributing to mik! This document provides guidelines and instructions for contributing.

## Development Setup

### Prerequisites

- Rust 1.89 or later (see MSRV policy below)
- `cargo-component` for building WASM components
- `wasm-tools` for inspecting WASM modules
- `wac` for WASM component composition

### Building

```bash
# Build the project
cargo build

# Build with OTLP tracing support
cargo build --features otlp

# Build in release mode
cargo build --release
```

### Testing

```bash
# Run all tests
cargo test --all

# Run tests with output
cargo test --all -- --nocapture
```

### Linting

```bash
# Run clippy
cargo clippy --all-features -- -D warnings

# Format code
cargo fmt --all

# Check dependencies for security vulnerabilities
cargo deny check
```

### Git Hooks Setup

We provide pre-commit hooks that run the same checks as CI. To enable them:

```bash
git config core.hooksPath .githooks
```

The pre-commit hook runs on Rust files only (skips docs, markdown, etc.):
- `cargo fmt --all -- --check` (formatting)
- `cargo clippy --all-features --all-targets -- -D warnings` (linting)
- `cargo check --all-features --all-targets` (compilation)

## Pull Request Process

1. **Fork the repository** and create your branch from `main`
2. **Create a feature branch** with a descriptive name:
   - `feat/add-new-feature`
   - `fix/resolve-issue-123`
   - `docs/update-readme`
3. **Write tests** for any new functionality
4. **Run the test suite** to ensure all tests pass
5. **Run clippy** and fix any warnings
6. **Update the changelog** in `CHANGELOG.md` under `[Unreleased]`
7. **Update documentation** if your changes affect public APIs or configuration
8. **Submit a pull request** with a clear description of your changes

## Commit Message Conventions

This project follows [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

### Types

| Type       | Description                                                   |
| ---------- | ------------------------------------------------------------- |
| `feat`     | A new feature                                                 |
| `fix`      | A bug fix                                                     |
| `docs`     | Documentation only changes                                    |
| `style`    | Changes that do not affect the meaning of the code            |
| `refactor` | A code change that neither fixes a bug nor adds a feature     |
| `perf`     | A code change that improves performance                       |
| `test`     | Adding missing tests or correcting existing tests             |
| `build`    | Changes that affect the build system or external dependencies |
| `ci`       | Changes to CI configuration files and scripts                 |
| `chore`    | Other changes that don't modify src or test files             |

### Examples

```
feat(runtime): add connection pooling for handlers
fix(script): handle null body in host.call response
docs: update configuration options in README
perf: enable parallel WASM compilation
test(cache): add LRU eviction tests
```

## Code Style

- Run `cargo fmt` before committing
- Follow existing code patterns and conventions
- Add doc comments for public APIs using `///`
- Include unit tests for new functionality
- Keep functions focused and reasonably sized
- Use meaningful variable and function names

### Documentation Comments

```rust
/// Executes a WASM handler with the given request.
///
/// # Arguments
///
/// * `module` - The name of the WASM module to execute
/// * `request` - The HTTP request to pass to the handler
///
/// # Returns
///
/// Returns the handler's HTTP response or an error if execution fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The module is not found
/// - The module fails to instantiate
/// - The handler returns an invalid response
pub fn execute_handler(module: &str, request: Request) -> Result<Response, Error> {
    // ...
}
```

## MSRV Policy

The Minimum Supported Rust Version (MSRV) is **1.89**.

- MSRV bumps require a minor version bump of mik
- MSRV is tested in CI
- New Rust features may be adopted after they've been stable for at least one release

## Security

If you discover a security vulnerability, please **do not** open a public issue.

Instead, report vulnerabilities through [GitHub Security Advisories](https://github.com/dufeutech/mik/security/advisories/new).

We take security issues seriously and will respond promptly to verified reports.

## Questions?

If you have questions about contributing, feel free to open a discussion or issue on GitHub.
