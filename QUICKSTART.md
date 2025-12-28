# mik Quick Start Guide

## Installation

```bash
cargo install mik
```

Or build from source:

```bash
cargo build --release -p mik
```

## Quick Start

```bash
# Create project
mik new my-service
cd my-service

# Build and run
mik build -rc
mik run
```

Test: `curl http://localhost:3000/run/my-service/`

## Commands Overview

| Command          | Purpose                              |
| ---------------- | ------------------------------------ |
| `mik new <name>` | Create new handler project           |
| `mik build`      | Build handler component              |
| `mik build -rc`  | Build release + compose with bridge  |
| `mik run`        | Run composed component               |
| `mik add <pkg>`  | Add OCI/git/path dependency          |

## Generated Project Structure

```
my-service/
├── Cargo.toml          # Component package
├── mik.toml            # Project config
├── wit/
│   ├── world.wit       # WIT world definition
│   └── deps/core/      # Core handler interface
├── src/
│   └── lib.rs          # Handler implementation
├── modules/            # Dependencies directory
└── .gitignore
```

## Handler Implementation

The generated `src/lib.rs` uses the `mik-sdk` macros:

```rust
#[allow(warnings)]
mod bindings;

use bindings::exports::mik::core::handler::{self, Guest, Response};
use mik_sdk::prelude::*;

routes! {
    GET "/" | "" => home,
    GET "/health" => health,
    GET "/users/:id" => get_user,
    POST "/users" => create_user,
}

fn home(_req: &Request) -> Response {
    ok!({
        "service": "my-service",
        "message": "Hello from mik!"
    })
}

fn health(_req: &Request) -> Response {
    ok!({ "status": "healthy" })
}

fn get_user(req: &Request) -> Response {
    let id = req.param("id").unwrap_or("unknown");
    ok!({ "id": id })
}

fn create_user(req: &Request) -> Response {
    // Parse JSON body
    let body: serde_json::Value = req.json().unwrap_or_default();
    ok!({
        "created": true,
        "data": body
    })
}
```

## Build Options

```bash
# Development build
mik build

# Release build
mik build -r

# Release + compose with bridge
mik build -rc
```

Output: `dist/<name>-composed.wasm`

## Running

```bash
# Run from mik.toml (multi-module mode)
mik run

# Run specific component (single component mode)
mik run dist/my-service-composed.wasm

# Run with multiple workers
mik run --workers 4
```

## Routing

All routes use `/run/<module>/*` pattern:

- **Single component**: `/run/<name>/*` (name from filename)
- **Multi-module**: `/run/<module>/*` (modules from `modules/` directory)

Built-in endpoints:
- `/health` - Health check
- `/metrics` - Prometheus metrics

Example: `mik run my-service-composed.wasm` → routes at `/run/my-service/*`

## mik.toml Configuration

```toml
[project]
name = "my-service"
version = "0.1.0"

[server]
port = 3000
modules = "modules/"

[composition]
http_handler = true  # Auto-downloads bridge from ghcr.io/dufeut/mik-sdk-bridge

[dependencies]
# OCI (ghcr.io is default)
"user/router" = "latest"
"user/auth" = "v1.0"

# Local path
utils = { path = "../utils" }
```

## Prerequisites

| Tool              | Install                         | Purpose               |
| ----------------- | ------------------------------- | --------------------- |
| `cargo-component` | `cargo install cargo-component` | Build WASI components |
| `wac`             | `cargo install wac-cli`         | Compose components    |

## Troubleshooting

### Build fails: "cargo-component not found"

```bash
cargo install cargo-component
```

### Compose fails: "wac not found"

```bash
cargo install wac-cli
```

### Routes return 404

Check the route pattern. All handler routes use `/run/<module>/*`:

```bash
# Correct
curl http://localhost:3000/run/my-service/
curl http://localhost:3000/run/my-service/health

# Built-in endpoints (no /run/ prefix)
curl http://localhost:3000/health
curl http://localhost:3000/metrics
```

## Next Steps

- Read the [mik-sdk documentation](https://github.com/dufeut/mik-sdk)
- Explore [examples](examples/)
- Learn about [WASI Preview 2](https://github.com/WebAssembly/WASI/tree/main/preview2)
