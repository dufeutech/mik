# ADR-006: Load Balancer Backend Selection Strategy

**Status:** Accepted
**Date:** 2025-12-30
**Decision Makers:** Project maintainer
**Context:** Algorithm for distributing requests across backend servers

---

## Summary

Implement round-robin as the default selection strategy with weighted round-robin available for backends with different capacities. Only healthy backends (circuit breaker not open) are considered for selection.

---

## Context

mik's load balancer distributes incoming requests across multiple backend servers. The selection algorithm determines which backend handles each request. The choice of algorithm affects:

1. **Load distribution**: How evenly traffic is spread
2. **Latency**: Overhead of selection decisions
3. **Fairness**: Whether all backends get appropriate traffic
4. **Capacity handling**: Ability to route more traffic to stronger backends

### Problem Statement

How should the load balancer select which backend server handles an incoming request?

---

## Decision Drivers

1. **Even distribution** - Traffic should be spread across backends
2. **Low overhead** - Selection should add minimal latency
3. **Health awareness** - Only route to healthy backends
4. **Capacity support** - Allow different backends to handle different loads
5. **Simplicity** - Easy to understand and debug
6. **Thread safety** - Support high-concurrency request handling

---

## Options Considered

### Option 1: Random Selection

Select a random healthy backend for each request.

```rust
fn select(&self, healthy: &[usize]) -> Option<usize> {
    if healthy.is_empty() {
        return None;
    }
    let idx = rand::thread_rng().gen_range(0..healthy.len());
    Some(healthy[idx])
}
```

**Pros:**
- Simplest implementation
- No shared state needed
- Naturally spreads load (probabilistically)

**Cons:**
- Uneven distribution with low request counts
- Can't guarantee fairness
- No deterministic behavior (hard to test/debug)
- Doesn't support weighted distribution well

### Option 2: Round-Robin (Selected as Default)

Select backends in circular order.

```rust
pub struct RoundRobin {
    current: AtomicUsize,
    total: usize,
}

impl Selection for RoundRobin {
    fn select(&self, healthy_indices: &[usize]) -> Option<usize> {
        if healthy_indices.is_empty() {
            return None;
        }
        let pos = self.current.fetch_add(1, Ordering::Relaxed) % self.total;
        let healthy_pos = pos % healthy_indices.len();
        Some(healthy_indices[healthy_pos])
    }
}
```

**Pros:**
- Even distribution guaranteed
- Deterministic behavior
- Very low overhead (single atomic increment)
- Easy to understand and debug
- Works well with similar-capacity backends

**Cons:**
- Doesn't account for backend capacity differences
- Doesn't consider current load on backends
- Counter can wrap (handled by modulo)

### Option 3: Weighted Round-Robin (Selected as Alternative)

Distribute traffic proportionally to backend weights.

```rust
pub struct WeightedRoundRobin {
    weights: Vec<u32>,      // Weight per backend
    current: AtomicUsize,
    total_weight: usize,
}

impl Selection for WeightedRoundRobin {
    fn select(&self, healthy_indices: &[usize]) -> Option<usize> {
        if healthy_indices.is_empty() {
            return None;
        }

        // Build expanded index list (backend appears weight times)
        let expanded = Self::compute_expanded_indices(&self.weights, healthy_indices);

        if expanded.is_empty() {
            return None;
        }

        let pos = self.current.fetch_add(1, Ordering::Relaxed) % expanded.len();
        Some(healthy_indices[expanded[pos]])
    }
}
```

Example with weights [2, 1, 3]:
- Backend 0: weight 2 (gets 2/6 = 33% traffic)
- Backend 1: weight 1 (gets 1/6 = 17% traffic)
- Backend 2: weight 3 (gets 3/6 = 50% traffic)

Virtual rotation: [0, 0, 1, 2, 2, 2]

**Pros:**
- Supports heterogeneous backend capacities
- Still deterministic and predictable
- Even distribution within weight ratios
- Low overhead

**Cons:**
- Requires weight configuration
- More complex than simple round-robin
- Weights are static (don't adapt to actual load)

### Option 4: Least Connections

Route to the backend with fewest active connections.

```rust
struct Backend {
    active_connections: AtomicU32,
}

fn select(&self, healthy: &[&Backend]) -> Option<usize> {
    healthy.iter()
        .enumerate()
        .min_by_key(|(_, b)| b.active_connections.load(Ordering::Relaxed))
        .map(|(i, _)| i)
}
```

**Pros:**
- Adapts to actual backend load
- Better for long-lived connections
- Naturally handles slow backends

**Cons:**
- Higher overhead (scan all backends)
- Connection tracking required
- Doesn't help with short-lived HTTP requests
- Can oscillate under certain conditions

### Option 5: Least Response Time

Route to the backend with lowest recent response time.

```rust
struct Backend {
    avg_response_time_ms: AtomicU64,
}
```

**Pros:**
- Routes to fastest backends
- Adapts to backend performance changes

**Cons:**
- Requires response time tracking
- Slow backends may never recover (no traffic to measure)
- Cold start problem for new backends
- Complex smoothing/averaging needed

---

## Decision

**Selected: Round-Robin (default) with Weighted Round-Robin (optional)**

### Rationale

1. **Round-robin for common case**: Most deployments have similar-capacity backends. Round-robin provides perfect distribution with minimal overhead.

2. **Weighted for capacity differences**: When backends have different capacities (e.g., 8-core vs 4-core), weighted round-robin allows proportional distribution without complex load measurement.

3. **Simplicity**: Both algorithms are deterministic and easy to understand. Operators can predict exactly how traffic will be distributed.

4. **Low overhead**: Single atomic increment per request. No locking, no scanning, no external state.

5. **Health-aware**: Both algorithms operate on the filtered list of healthy backends (circuit breaker check happens before selection).

6. **Thread-safe**: Atomic operations ensure correct behavior under high concurrency.

---

## Consequences

### Positive

- Predictable, even distribution across backends
- Minimal selection overhead
- Easy to understand and debug
- Works well for HTTP request/response workloads
- Weighted option for heterogeneous capacity

### Negative

- Doesn't adapt to real-time backend load
- Weights are static (require reconfiguration to change)
- Round-robin may overload slow backends

### Neutral

- Connection/response time tracking not implemented
- More sophisticated algorithms could be added later

---

## Implementation Details

### Selection Trait

```rust
pub(super) trait Selection: Send + Sync {
    /// Select the next backend index from the available backends.
    fn select(&self, healthy_indices: &[usize]) -> Option<usize>;
}
```

### Round-Robin Implementation

```rust
#[derive(Debug)]
pub struct RoundRobin {
    current: AtomicUsize,
    total: usize,
}

impl RoundRobin {
    pub fn new(total: usize) -> Self {
        Self {
            current: AtomicUsize::new(0),
            total,
        }
    }
}

impl Selection for RoundRobin {
    fn select(&self, healthy_indices: &[usize]) -> Option<usize> {
        if healthy_indices.is_empty() {
            return None;
        }

        let pos = self.current.fetch_add(1, Ordering::Relaxed) % self.total;
        let healthy_pos = pos % healthy_indices.len();
        Some(healthy_indices[healthy_pos])
    }
}
```

### Weighted Round-Robin Implementation

```rust
#[derive(Debug)]
pub struct WeightedRoundRobin {
    weights: Vec<u32>,
    current: AtomicUsize,
    total_weight: usize,
}

impl WeightedRoundRobin {
    pub fn new(weights: Vec<u32>) -> Self {
        // Ensure minimum weight of 1
        let weights: Vec<u32> = weights.into_iter().map(|w| w.max(1)).collect();
        let total_weight: usize = weights.iter().map(|&w| w as usize).sum();

        Self {
            weights,
            current: AtomicUsize::new(0),
            total_weight,
        }
    }

    /// Create expanded indices for weighted selection.
    ///
    /// With weights [2, 1], produces [0, 0, 1] so backend 0
    /// appears twice as often as backend 1.
    fn compute_expanded_indices(weights: &[u32], indices: &[usize]) -> Vec<usize> {
        let mut expanded = Vec::new();
        for (i, &idx) in indices.iter().enumerate() {
            let weight = weights.get(idx).copied().unwrap_or(1) as usize;
            for _ in 0..weight {
                expanded.push(i);
            }
        }
        expanded
    }
}

impl Selection for WeightedRoundRobin {
    fn select(&self, healthy_indices: &[usize]) -> Option<usize> {
        if healthy_indices.is_empty() {
            return None;
        }

        let expanded = Self::compute_expanded_indices(&self.weights, healthy_indices);

        if expanded.is_empty() {
            return None;
        }

        let pos = self.current.fetch_add(1, Ordering::Relaxed) % expanded.len();
        Some(healthy_indices[expanded[pos]])
    }
}
```

### Integration with Health Checks

Selection operates on filtered healthy backends:

```rust
fn select_backend(&self) -> Option<&Backend> {
    // Filter to healthy backends (circuit breaker not open)
    let healthy_indices: Vec<usize> = self.backends.iter()
        .enumerate()
        .filter(|(_, b)| b.circuit_breaker.is_available())
        .map(|(i, _)| i)
        .collect();

    // Apply selection algorithm
    self.selector.select(&healthy_indices)
        .map(|i| &self.backends[i])
}
```

---

## Configuration Examples

### Equal Capacity (Round-Robin)

```toml
[load_balancer]
strategy = "round-robin"

[[backends]]
url = "http://backend-1:3000"

[[backends]]
url = "http://backend-2:3000"

[[backends]]
url = "http://backend-3:3000"
```

All backends receive equal traffic (33% each).

### Different Capacities (Weighted)

```toml
[load_balancer]
strategy = "weighted-round-robin"

[[backends]]
url = "http://backend-large:3000"
weight = 4  # 8-core machine

[[backends]]
url = "http://backend-medium:3000"
weight = 2  # 4-core machine

[[backends]]
url = "http://backend-small:3000"
weight = 1  # 2-core machine
```

Traffic distribution:
- backend-large: 4/7 = 57%
- backend-medium: 2/7 = 29%
- backend-small: 1/7 = 14%

---

## Edge Cases

### All Backends Unhealthy

When all circuit breakers are open:
```rust
fn select(&self, healthy_indices: &[usize]) -> Option<usize> {
    if healthy_indices.is_empty() {
        return None;  // No backend available
    }
    // ...
}
```

The load balancer returns 503 Service Unavailable.

### Single Backend

Round-robin with one backend always returns that backend:
```rust
let rr = RoundRobin::new(1);
assert_eq!(rr.select(&[0]), Some(0));
assert_eq!(rr.select(&[0]), Some(0));
```

### Backend Becomes Unhealthy

When backend 1 (of 3) becomes unhealthy:
- Before: [0, 1, 2, 0, 1, 2, ...]
- After: [0, 2, 0, 2, ...] (backend 1 skipped)

Traffic automatically redistributes to remaining healthy backends.

---

## Observability

Selection decisions should be tracked:

```rust
// Metrics
backend_selections_total{backend="backend-1"} 15432
backend_selections_total{backend="backend-2"} 15428
backend_selections_total{backend="backend-3"} 15430

// No healthy backends events
load_balancer_no_backends_available_total 3
```

---

## Alternatives Considered But Rejected

| Algorithm | Reason Not Selected |
|-----------|---------------------|
| Random | Uneven distribution with low request counts; unpredictable |
| Least connections | Higher overhead; doesn't help with short HTTP requests |
| Least response time | Complex tracking; cold start problems; can starve slow backends |
| Consistent hashing | Overkill for small backend pools; adds complexity |

---

## Future Considerations

If needed, more sophisticated algorithms could be added:

1. **Power of Two Choices**: Randomly select two backends, pick the less loaded one
2. **Adaptive Weights**: Adjust weights based on observed response times
3. **Geographic/Zone Awareness**: Prefer backends in the same zone

These would be additive (new Selection implementations), not replacements.

---

## Related Decisions

- ADR-005: Circuit Breaker Design (determines which backends are healthy)

---

## References

- [Load Balancing Algorithms (NGINX)](https://docs.nginx.com/nginx/admin-guide/load-balancer/http-load-balancer/)
- [Round-Robin Scheduling](https://en.wikipedia.org/wiki/Round-robin_scheduling)
- [The Power of Two Random Choices](https://www.eecs.harvard.edu/~michaelm/postscripts/handbook2001.pdf)

---

## Changelog

| Date | Change |
|------|--------|
| 2025-12-30 | Initial decision documented |
