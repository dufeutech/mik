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

## Documentation

Full documentation: [dufeutech.github.io/mik](https://dufeutech.github.io/mik)

## License

MIT
