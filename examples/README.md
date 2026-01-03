# mik Examples

This directory contains educational examples demonstrating how to use the mik
WASI HTTP runtime and its components.

## Examples Overview

| Example | Description | Key Concepts |
|---------|-------------|--------------|
| [01_hello_world.rs](./01_hello_world.rs) | Minimal mik setup | `HostBuilder`, basic configuration |
| [02_config_parsing.rs](./02_config_parsing.rs) | Configuration handling | `Config`, `Manifest`, validation |
| [03_circuit_breaker.rs](./03_circuit_breaker.rs) | Reliability pattern | `CircuitBreaker`, state transitions |
| [04_kv_store.rs](./04_kv_store.rs) | Key-value storage | `KvStore`, CRUD, TTL |

## Running Examples

Examples can be run using cargo:

```bash
# Run a specific example
cargo run --example 01_hello_world

# Run with release optimizations
cargo run --release --example 01_hello_world
```

## Prerequisites

Before running examples, ensure you have:

1. Rust 1.89+ installed (Rust 2024 edition)
2. The project built successfully: `cargo build`

## Example Categories

### Getting Started

- **01_hello_world** - Start here! Shows the simplest possible mik setup.

### Configuration

- **02_config_parsing** - Learn how to parse and validate mik configuration files.
  Covers both the legacy `mikrozen.toml` format and the modern `mik.toml` manifest.

### Reliability

- **03_circuit_breaker** - Implement resilient service calls using the circuit
  breaker pattern. Shows how to protect your handlers from cascading failures.

### Services

- **04_kv_store** - Use the embedded key-value store for caching and session
  data. Demonstrates CRUD operations and TTL-based expiration.

## Architecture Reference

These examples demonstrate different layers of the mik architecture:

```
+-------------------------------------------------------------+
|  Layer 1: Scripts (JS/TS Orchestration)                     |
|  - host.call() to invoke handlers                           |
|  - NO network, filesystem, or imports                       |
+-------------------------------------------------------------+
|  Layer 2: Handlers (WASM Business Logic)                    |
|  - HTTP client, JSON processing                             |
|  - Access services via sidecar only                         |
+-------------------------------------------------------------+
|  Layer 3: Sidecars (Infrastructure)                         |
|  - Storage, SQL, KV services                                |
|  - Full network and credential access                       |
+-------------------------------------------------------------+
```

## Further Reading

- [Project README](../README.md) - Project overview
- [CLAUDE.md](../CLAUDE.md) - Development guidelines
- [Architecture Decision Records](../docs-dev/adr/) - Design decisions

## Contributing

When adding new examples:

1. Use the naming convention `NN_name.rs` (e.g., `05_new_feature.rs`)
2. Include comprehensive doc comments explaining the purpose
3. Add inline comments for non-obvious code
4. Update this README with the new example
5. Ensure the example compiles and runs successfully
