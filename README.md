# mik

Package Manager and Runtime for WASI HTTP Components.

A Rust-based CLI tool that serves as the host environment for running portable WebAssembly components. Think Node.js or Python runtime, but for WASI HTTP components.

## Install

**Linux/macOS:**
```bash
curl -fsSL https://raw.githubusercontent.com/dufeutech/mik/main/install.sh | bash
```

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/dufeutech/mik/main/install.ps1 | iex
```

**Or via cargo:**
```bash
cargo install mik
```

## Quick Start

```bash
# Create a new project
mik new my-service
cd my-service

# Build and run
mik build -rc
mik dev
```

Test your API:
```bash
curl http://localhost:3000/run/my-service/
```

## Features

- **WASI HTTP Runtime** - Run portable WebAssembly components
- **Hot Reload** - Auto-rebuild on source changes
- **Embedded Services** - KV, SQL, Storage, Cron built-in
- **Multi-worker** - Horizontal scaling with load balancer
- **AOT Compilation** - Fast startup with cached compilation
- **Multi-tenant Routing** - Isolated tenant modules with `/tenant/<id>/` routing
- **Shell Completions** - Tab completion for bash, zsh, fish, PowerShell

## Commands

```bash
mik new <name>        # Create project
mik build [-rc]       # Build component (release + compose)
mik dev               # Dev server with watch mode
mik run               # Run in foreground
mik run --detach      # Run as background instance
mik ps                # List instances
mik stop [name]       # Stop instance
mik completions       # Generate shell completions
```

## Routing

### Platform Modules

Platform modules are shared across all tenants and accessible via `/run/<module>/`:

```
modules/
├── auth.wasm        → /run/auth/*
├── payments.wasm    → /run/payments/*
└── notifications.wasm → /run/notifications/*
```

### Multi-Tenant Routing

Enable tenant isolation by setting `user_modules` in `mik.toml`:

```toml
[server]
modules = "modules/"
user_modules = "user-modules/"
```

Tenant modules are accessible via `/tenant/<tenant-id>/<module>/`:

```
user-modules/
├── acme-corp/
│   ├── orders.wasm     → /tenant/acme-corp/orders/*
│   └── inventory.wasm  → /tenant/acme-corp/inventory/*
└── globex/
    └── orders.wasm     → /tenant/globex/orders/*
```

Each tenant can only access their own modules - cross-tenant access returns 404.

## Gateway API

The runtime exposes discovery endpoints for gateway integration:

```bash
# List all handlers (platform + tenant)
curl http://localhost:3000/_mik/handlers

# Aggregated OpenAPI spec for platform
curl http://localhost:3000/_mik/openapi/platform

# Aggregated OpenAPI spec for a tenant
curl http://localhost:3000/_mik/openapi/tenant/acme-corp
```

## Documentation

Full documentation: [dufeutech.github.io/mik](https://dufeutech.github.io/mik)

## License

MIT
