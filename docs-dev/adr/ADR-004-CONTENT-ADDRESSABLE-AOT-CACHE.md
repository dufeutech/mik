# ADR-004: Content-Addressable AOT Cache

**Status:** Accepted
**Date:** 2025-12-30
**Decision Makers:** Project maintainer
**Context:** AOT compilation cache strategy for WASM components

---

## Summary

Use content-addressable (BLAKE3 hash-based) caching for AOT-compiled WASM components instead of time-based (mtime) caching.

---

## Context

mik compiles WASM components to native code using wasmtime's ahead-of-time (AOT) compilation. This compilation is expensive (100ms-2s depending on component size) and should be cached to avoid repeated work.

The cache must:
- Correctly detect when recompilation is needed
- Handle component updates from any source (local build, OCI pull, git)
- Work across wasmtime version upgrades
- Support concurrent access from multiple mik instances

### Problem Statement

How should mik determine if a cached AOT artifact is valid for a given WASM component?

---

## Decision Drivers

1. **Correctness** - Never serve stale artifacts that don't match source
2. **Reliability** - Work correctly regardless of how the component was obtained
3. **Cross-environment** - Work when components are copied between machines
4. **Simplicity** - Minimal edge cases and special handling
5. **Performance** - Cache lookups should be fast

---

## Options Considered

### Option 1: Time-Based Caching (mtime comparison)

Store AOT artifacts alongside source files, validate by comparing modification times.

```
modules/
  hello.wasm         (mtime: 2024-01-15 10:00:00)
  hello.wasm.aot     (mtime: 2024-01-15 10:00:01)
```

Cache is valid if `hello.wasm.aot` is newer than `hello.wasm`.

**Pros:**
- Simple implementation (compare two timestamps)
- No hashing overhead
- Intuitive model (newer = valid)
- Files stay together

**Cons:**
- **Unreliable with git**: Git doesn't preserve mtime; checkout sets current time
- **Breaks with copy**: `cp` and `scp` often reset mtime
- **OCI registries**: Pulled components have arbitrary timestamps
- **Clock skew**: NFS, VMs, containers may have clock drift
- **Race conditions**: Build might complete between checks

Example failure:
```bash
# On machine A
cargo component build  # hello.wasm mtime = 10:00:00
mik build              # hello.wasm.aot cached

# Copy to machine B
scp hello.wasm server:modules/  # mtime reset to 10:05:00
# mik on server thinks component is newer, recompiles unnecessarily
```

### Option 2: Embedded Metadata

Embed source hash or version in the AOT artifact header.

```rust
struct AotHeader {
    source_hash: [u8; 32],
    wasmtime_version: String,
    compiled_at: SystemTime,
}
```

**Pros:**
- Self-describing artifacts
- Can validate without source file
- Can include additional metadata

**Cons:**
- Custom binary format to maintain
- Must parse header on every load
- Still requires computing source hash
- Artifact format tied to implementation

### Option 3: Content-Addressable Cache (Selected)

Use BLAKE3 hash of WASM content as cache key. Store artifacts in a global cache directory.

```
~/.mik/
  cache/
    aot/
      v1/                           # Cache format version
        wasmtime-40/                # Wasmtime version isolation
          ab12cd34ef56...7890.aot   # BLAKE3 hash -> compiled artifact
```

Cache lookup:
```rust
let hash = blake3::hash(&wasm_bytes);
let cache_key = hex::encode(&hash[..16]);  // 128-bit, 32 hex chars
let aot_path = cache_dir.join(format!("{cache_key}.aot"));

if aot_path.exists() {
    // Cache hit - content matches by definition
    return load_from_cache(aot_path);
}
```

**Pros:**
- **Immune to time manipulation**: Hash depends only on content
- **Works with any source**: Local build, OCI pull, git clone, scp
- **Automatic deduplication**: Same component from different paths shares cache
- **Version isolation**: Different wasmtime versions use different directories
- **Shareable**: Cache could be synced between machines
- **Simple validation**: If hash matches, content matches (by definition)

**Cons:**
- Must read entire file to compute hash
- Global cache directory requires cleanup strategy
- Hash computation overhead (~50MB/s on modern hardware)

---

## Decision

**Selected: Option 3 - Content-Addressable Cache**

### Rationale

1. **Reliability over performance**: The hash computation overhead (~10-20ms for a 1MB component) is negligible compared to AOT compilation time (~500ms-2s). Correctness is more important.

2. **Universal source compatibility**: Components arrive via multiple paths:
   - `cargo component build` (local)
   - `mik add pkg/name` (OCI registry)
   - `git clone` (version control)
   - Manual copy (`scp`, `cp`, download)

   Content-addressable caching works correctly for all of these.

3. **Git workflow compatibility**: Development teams use git. Time-based caching would cause constant cache misses after `git checkout` or `git pull`.

4. **Automatic invalidation**: When wasmtime is upgraded, the version-isolated directory structure automatically invalidates all cached artifacts without explicit cache clearing.

5. **Deduplication**: When the same handler is used in multiple projects, or when the same component is loaded from different paths, only one cached artifact exists.

---

## Consequences

### Positive

- Cache is reliable regardless of how components are obtained
- Works correctly with git, OCI registries, and file copies
- Automatic wasmtime version isolation
- Natural deduplication of identical components
- No special cases or edge condition handling

### Negative

- Must read full file content to compute hash (I/O overhead)
- Global cache requires periodic cleanup (LRU eviction)
- Cannot validate cache without source file content

### Neutral

- ~10-20ms hash computation overhead per component (acceptable)
- Cache location is fixed (`~/.mik/cache/aot/`)

---

## Implementation Details

### Cache Structure

```
~/.mik/
  cache/
    aot/
      v1/                          # Increment on format changes
        wasmtime-40/               # Wasmtime major version
          ab12cd34...7890.aot      # Truncated BLAKE3 hash
```

### Hash Computation

Uses BLAKE3 for speed (~1GB/s on modern CPUs):

```rust
pub fn compute_key(wasm_bytes: &[u8]) -> String {
    let hash = blake3::hash(wasm_bytes);
    // Truncate to 128 bits (32 hex chars) for shorter filenames
    hex::encode(&hash.as_bytes()[..16])
}
```

128-bit truncation provides:
- Collision probability: 1 in 2^64 for birthday attack
- Shorter filenames (32 chars vs 64 chars)
- Still effectively unique for practical purposes

### Atomic Writes

Cache writes use temp file + rename for atomicity:

```rust
pub fn put(&self, wasm_bytes: &[u8], compiled: &[u8]) -> Result<PathBuf> {
    let key = Self::compute_key(wasm_bytes);
    let aot_path = self.cache_dir.join(format!("{key}.aot"));
    let temp_path = self.cache_dir.join(format!("{key}.aot.tmp"));

    fs::write(&temp_path, compiled)?;
    fs::rename(&temp_path, &aot_path)?;  // Atomic on POSIX

    Ok(aot_path)
}
```

### Cache Cleanup (LRU)

When cache exceeds size limit (default 1GB), remove oldest entries by access time:

```rust
pub fn cleanup(&self) -> Result<CleanupStats> {
    let mut entries = collect_entries_with_atime()?;
    entries.sort_by_key(|e| e.accessed);  // Oldest first

    while total_size > self.config.max_size_bytes {
        remove_oldest(&mut entries)?;
    }
}
```

### Bypass Mode

For hot-reload during development, cache can be bypassed:

```rust
let cache = if watch_mode {
    AotCache::bypass()  // Always recompile
} else {
    AotCache::new(config)?
};
```

---

## Alternatives Considered But Rejected

| Approach | Reason Not Selected |
|----------|---------------------|
| mtime comparison | Unreliable with git, copy, OCI; clock skew issues |
| Embedded metadata | Still requires hash; custom format maintenance |
| CRC32/MD5 | Weaker collision resistance; BLAKE3 is faster anyway |
| Per-directory cache | No deduplication; messy project directories |

---

## Related Decisions

- ADR-003: Bridge Composition (composed components are also cached)

---

## References

- [BLAKE3 Specification](https://github.com/BLAKE3-team/BLAKE3-specs)
- [Content-addressable storage](https://en.wikipedia.org/wiki/Content-addressable_storage)
- [Wasmtime Cache](https://docs.wasmtime.dev/cli-cache.html)

---

## Changelog

| Date | Change |
|------|--------|
| 2025-12-30 | Initial decision documented |
