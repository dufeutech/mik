# Example Configurations

This directory contains example `mik.toml` configurations for different use cases.

## Available Examples

| File | Use Case |
|------|----------|
| `minimal.toml` | Simplest possible configuration |
| `multi-module.toml` | Multiple WASM handlers in one project |
| `multi-tenant.toml` | SaaS with per-tenant module isolation |
| `production.toml` | Production deployment with all settings tuned |
| `with-scripts.toml` | JavaScript orchestration layer |
| `with-load-balancer.toml` | Integrated L7 load balancing |

## Usage

Copy the relevant example to your project root:

```bash
# From your project directory
cp path/to/mik/examples/configs/minimal.toml ./mik.toml

# Or use mik new to generate a default config
mik new my-project
```

## Configuration Reference

See the full configuration documentation at:
- [Configuration Guide](/guides/configuration)
- [Multi-Tenant Routing](/guides/multi-tenant)
- [Production Guide](/guides/production)
