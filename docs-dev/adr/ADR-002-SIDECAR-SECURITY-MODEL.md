# ADR-002: Sidecar Security Model

**Status:** Accepted
**Date:** 2025-12-27
**Decision Makers:** Project maintainer
**Context:** Infrastructure access pattern for WASM handlers

---

## Summary

Handlers access infrastructure (databases, storage, external APIs) exclusively via HTTP calls to sidecar processes. Credentials never enter the WASM sandbox.

---

## Context

WASM handlers need to interact with infrastructure:
- Databases (PostgreSQL, SQLite)
- Key-value stores (Redis)
- Object storage (S3, MinIO)
- External APIs (payment processors, email services)

These services require credentials (passwords, API keys, connection strings). We need a pattern that:
- Keeps credentials secure
- Maintains WASM portability
- Provides clear security boundaries
- Enables policy enforcement

### Problem Statement

How should WASM handlers access infrastructure services while:
- Never exposing credentials to WASM code
- Maintaining handler portability across environments
- Enabling centralized policy enforcement (audit logging)

---

## Decision Drivers

1. **Credential isolation** - Credentials must never enter WASM memory
2. **Portability** - Handlers should work across environments without modification
3. **Policy enforcement** - Centralized place for rate limits, ACLs, audit logs
4. **Operational simplicity** - Standard HTTP debugging, monitoring, tracing
5. **Defense in depth** - Multiple security layers, not just WASM sandbox

---

## Options Considered

### Option 1: Direct Database Connections from WASM

Give handlers database drivers and connection strings.

```rust
// handler.rs
fn handle(req: Request) -> Response {
    let db = postgres::connect(env::var("DATABASE_URL"))?;
    let users = db.query("SELECT * FROM users")?;
    // ...
}
```

**Pros:**
- Direct, low-latency access
- Familiar pattern for developers
- No additional infrastructure

**Cons:**
- Credentials in WASM memory (extractable via memory dump)
- Each handler needs driver dependencies (bloats WASM)
- No centralized policy enforcement
- Connection pooling per-handler (inefficient)
- Different handlers may use different driver versions
- Hard to audit all database access

### Option 2: WASI Capability Imports

Define WASI interfaces for infrastructure access.

```wit
// wasi-sql.wit
interface sql {
    query: func(sql: string, params: list<value>) -> result<rows, error>;
    execute: func(sql: string, params: list<value>) -> result<u64, error>;
}
```

Host provides implementation that handles credentials.

**Pros:**
- Type-safe interface
- Credentials stay in host
- Standard WASI pattern

**Cons:**
- Must define interfaces for every service type
- Interface changes require handler recompilation
- Complex to implement per-database quirks
- Not a standard WASI interface yet (custom extension)
- Tight coupling between host and handler versions

### Option 3: HTTP-Only Sidecar Pattern (Selected)

Handlers make HTTP calls to sidecar processes. Sidecars hold credentials and provide REST APIs.

```
┌─────────────────────────────────────────────────────────┐
│  mik runtime                                            │
│  ┌─────────────┐                                        │
│  │   Handler   │ ──HTTP──▶ mikcar (sidecar)             │
│  │   (WASM)    │           │                            │
│  │             │           ├── /kv/:key                 │
│  │ No credentials          ├── /sql/query               │
│  │ No raw sockets          ├── /storage/:path           │
│  │ HTTP only                                             │
│  └─────────────┘                    │                   │
└─────────────────────────────────────│───────────────────┘
                                      │
                    ┌─────────────────┼─────────────────┐
                    ▼                 ▼                 ▼
               PostgreSQL          Redis            MinIO
               (credentials)    (credentials)    (credentials)
```

```rust
// handler.rs - NO credentials, just HTTP
fn handle(req: Request) -> Response {
    let users: Vec<User> = http::post("http://mikcar:9001/sql/query")
        .json(&json!({
            "sql": "SELECT * FROM users WHERE active = $1",
            "params": [true]
        }))
        .send()?
        .json()?;
    // ...
}
```

**Pros:**
- Credentials never enter WASM
- Standard HTTP (debuggable, traceable, cacheable)
- Sidecar can enforce policies (rate limits, row-level security)
- Sidecar can audit all access
- Handler remains portable (just HTTP calls)
- Sidecar can be shared across handlers
- Connection pooling handled by sidecar
- Can swap sidecar implementations without handler changes

**Cons:**
- HTTP overhead vs direct connection
- Additional process to deploy
- Network hop latency
- Must serialize/deserialize data

### Option 4: Embedded Services in Host

Build infrastructure adapters directly into mik runtime.

```toml
# mik.toml
[services.sql]
driver = "postgres"
url = "postgres://..."

[services.kv]
driver = "redis"
url = "redis://..."
```

Handlers call via host functions:
```rust
let result = host::sql_query("SELECT ...", params)?;
```

**Pros:**
- Single binary deployment
- No network hop
- Credentials in host config

**Cons:**
- mik becomes responsible for all driver maintenance
- Bloats mik binary with drivers
- Cannot scale services independently
- Harder to test handlers in isolation
- Tight coupling to mik internals

---

## Decision

**Selected: Option 3 - HTTP-Only Sidecar Pattern**

### Rationale

1. **Credentials stay external**: WASM memory can theoretically be dumped/inspected. Keeping credentials in a separate process with proper access controls is more secure.

2. **Policy enforcement point**: Sidecar is the natural place for:
   - Rate limiting per-handler
   - Row-level security
   - Query allowlisting
   - Audit logging
   - Cost tracking

3. **Operational familiarity**: HTTP is universally understood. Teams can:
   - Debug with curl
   - Monitor with standard tools
   - Trace with OpenTelemetry
   - Cache with standard proxies

4. **Handler portability**: Handlers just make HTTP calls. They work with:
   - mikcar (our sidecar)
   - Any REST API
   - Mock servers for testing
   - Different backends (swap Redis for DynamoDB)

5. **Independent scaling**: Sidecar can be scaled separately from handlers. Heavy database workloads don't require scaling the WASM runtime.

6. **Defense in depth**: Even if a handler is compromised:
   - It has no credentials
   - It can only make HTTP calls
   - Sidecar enforces what operations are allowed
   - All access is logged

---

## Consequences

### Positive

- Clear security boundary (credentials never in WASM)
- Auditable access (all operations go through sidecar)
- Testable handlers (mock the HTTP endpoints)
- Swappable backends (handlers don't care what's behind the API)
- Policy enforcement at sidecar level
- Standard debugging tools (curl, Postman, etc.)

### Negative

- Additional latency (HTTP round-trip vs direct connection)
- Additional deployment complexity (sidecar process)
- Serialization overhead (JSON vs binary protocols)
- Must maintain sidecar API compatibility

### Neutral

- Different mental model (infrastructure as HTTP services)
- Requires sidecar for local development

---

## Three-Layer Capability Model

This decision establishes a capability model with defense in depth:

| Layer | Network Access | Capabilities | Trust Level |
|-------|----------------|--------------|-------------|
| **Scripts** | None | `host.call()` only | Untrusted |
| **Handlers** | HTTP only | `mik_sdk::http_client` | Semi-trusted |
| **Sidecars** | Full | Native networking | Trusted |

```
Request
   ↓
Script (no network) ──host.call()──▶ Handler (HTTP only) ──HTTP──▶ Sidecar ──▶ Database
                                                                      │
                                                              Credentials here
                                                              Policies here
                                                              Audit logs here
```

### Layer Details

**Scripts (Untrusted):**
- Execute in rquickjs sandbox
- Cannot make any network calls
- Can only invoke handlers via `host.call()`
- Used for orchestration, not business logic

**Handlers (Semi-trusted):**
- Execute in WASM sandbox with memory limits
- Can make HTTP calls (controlled by `http_allowed` config)
- Cannot access filesystem, raw sockets, or spawn processes
- Business logic lives here

**Sidecars (Trusted):**
- Native processes with full system access
- Hold all credentials
- Enforce access policies
- Provide HTTP APIs for infrastructure

---

## Implementation: mikcar

The reference sidecar implementation (`mikcar`) provides:

| Endpoint | Service | Backend |
|----------|---------|---------|
| `GET/PUT/DELETE /kv/:key` | Key-Value | Redis |
| `POST /sql/query` | SQL Query | PostgreSQL/SQLite |
| `POST /sql/execute` | SQL Execute | PostgreSQL/SQLite |
| `GET/PUT/DELETE /storage/*path` | Object Storage | S3/MinIO/Local |
| `POST /email/send` | Email | SMTP |

### Configuration

```bash
# mikcar holds all credentials
mikcar --all \
  --postgres-url "postgres://user:pass@db:5432/app" \
  --redis-url "redis://:pass@redis:6379" \
  --s3-endpoint "http://minio:9000" \
  --s3-access-key "..." \
  --s3-secret-key "..."
```

Handlers just call:
```rust
http::post("http://mikcar:9001/sql/query")
    .json(&json!({"sql": "SELECT ...", "params": [...]}))
```

---

## Security Considerations

### What if a handler is compromised?

1. **No credentials to steal** - Handler never has database passwords
2. **Limited blast radius** - Can only access APIs exposed by sidecar
3. **Audit trail** - All operations logged at sidecar
4. **Policy enforcement** - Sidecar can restrict operations

### What if the sidecar is compromised?

The sidecar runs as a native process with proper OS-level protections:
- Run as non-root user
- Use network policies (only handlers can reach it)
- Enable audit logging
- Rotate credentials regularly

### Network Security

```
┌─────────────────────┐     ┌─────────────────────┐
│   mik (handlers)    │────▶│   mikcar (sidecar)  │
│   Port 3000         │     │   Port 9001         │
│                     │     │                     │
│   Exposed to        │     │   Internal only     │
│   external traffic  │     │   (network policy)  │
└─────────────────────┘     └─────────────────────┘
```

Sidecar should NOT be exposed to external traffic. Use network policies:
```yaml
# Kubernetes NetworkPolicy
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: mikcar-ingress
spec:
  podSelector:
    matchLabels:
      app: mikcar
  ingress:
    - from:
        - podSelector:
            matchLabels:
              app: mik
```

---

## Alternatives Not Selected

| Option | Reason Not Selected |
|--------|---------------------|
| Direct DB connections | Credentials in WASM memory; no policy enforcement |
| WASI capability imports | Non-standard; tight coupling; complex to maintain |
| Embedded services | Bloats runtime; cannot scale independently |

---

## Related Decisions

- ADR-001: JS Orchestration (scripts have even less access than handlers)
- ADR-003: Bridge Composition (handler interface design)

---

## References

- [Dapr Sidecar Pattern](https://docs.dapr.io/concepts/dapr-services/sidecar/)
- [Envoy Proxy](https://www.envoyproxy.io/)
- [WASM Security Model](https://webassembly.org/docs/security/)
- [12-Factor App: Backing Services](https://12factor.net/backing-services)

---

## Changelog

| Date | Change |
|------|--------|
| 2025-12-27 | Initial decision documented |
