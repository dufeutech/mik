# ADR-005: Circuit Breaker State Machine Design

**Status:** Accepted
**Date:** 2025-12-30
**Decision Makers:** Project maintainer
**Context:** Reliability pattern for backend server failures in load balancer

---

## Summary

Implement a three-state circuit breaker (Closed, Open, Half-Open) with configurable thresholds to prevent cascading failures when backend servers become unhealthy.

---

## Context

mik's load balancer routes requests to multiple backend servers. When a backend fails (network errors, timeouts, 5xx responses), continuing to send traffic to it:

1. Wastes resources on requests that will fail
2. Increases latency for users (waiting for timeouts)
3. Can cascade failures to healthy backends (overload)
4. Prevents the failing backend from recovering (continuous load)

The circuit breaker pattern addresses this by temporarily "opening" the circuit to failing backends, allowing them time to recover.

### Problem Statement

How should the load balancer detect and handle failing backends to:
- Stop sending traffic to unhealthy backends quickly
- Allow backends to recover before resuming traffic
- Avoid false positives from transient errors
- Provide predictable behavior under failure conditions

---

## Decision Drivers

1. **Fast failure detection** - Stop sending traffic to broken backends quickly
2. **Recovery opportunity** - Give backends time to recover
3. **False positive resistance** - Don't open circuit on transient errors
4. **Predictable behavior** - Clear state machine with documented transitions
5. **Thread safety** - Support concurrent request handling
6. **Configurability** - Allow tuning for different failure patterns

---

## Options Considered

### Option 1: Simple Failure Counter

Track consecutive failures, disable backend when threshold exceeded.

```rust
struct Backend {
    consecutive_failures: AtomicU32,
    disabled_until: Option<Instant>,
}

fn record_failure(&self) {
    let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
    if failures >= THRESHOLD {
        self.disabled_until = Some(Instant::now() + DISABLE_DURATION);
    }
}
```

**Pros:**
- Simple implementation
- Low overhead

**Cons:**
- No gradual recovery (all-or-nothing)
- Single failure after recovery disables again immediately
- No probe mechanism to test recovery

### Option 2: Three-State Circuit Breaker (Selected)

Classic circuit breaker with Closed, Open, and Half-Open states.

```
┌────────────────────────────────────────────────────────────┐
│                                                            │
│  ┌──────────┐    failures >= threshold    ┌──────────┐    │
│  │  Closed  │ ────────────────────────────▶│   Open   │    │
│  └──────────┘                              └──────────┘    │
│       ▲                                         │          │
│       │                                         │          │
│       │ successes >= threshold            timeout elapsed  │
│       │                                         │          │
│       │         ┌────────────┐                  │          │
│       └─────────│  HalfOpen  │◀─────────────────┘          │
│                 └────────────┘                             │
│                       │                                    │
│                       │ failure                            │
│                       ▼                                    │
│                   ┌──────────┐                             │
│                   │   Open   │ (reset timer)               │
│                   └──────────┘                             │
└────────────────────────────────────────────────────────────┘
```

**States:**
- **Closed**: Normal operation, requests flow through
- **Open**: Backend failing, requests blocked
- **Half-Open**: Testing recovery, limited requests allowed

**Pros:**
- Gradual recovery via half-open state
- Proven pattern with well-understood behavior
- Configurable thresholds for both failure and recovery
- Natural probe mechanism for recovery testing

**Cons:**
- More complex than simple counter
- Three states to manage and test

### Option 3: Sliding Window Rate-Based

Track failure rate over a time window, open circuit when rate exceeds threshold.

```rust
struct CircuitBreaker {
    window: RingBuffer<(Instant, bool)>,  // (timestamp, is_success)
    window_size: Duration,
}

fn failure_rate(&self) -> f64 {
    let recent = self.window.iter()
        .filter(|(t, _)| t.elapsed() < self.window_size);
    let (total, failures) = recent.fold(...);
    failures as f64 / total as f64
}
```

**Pros:**
- Considers failure rate, not just consecutive failures
- Resistant to isolated failures
- More nuanced decision making

**Cons:**
- Higher memory overhead (stores all requests in window)
- More complex implementation
- Harder to reason about transitions
- May react slowly to sudden failures

### Option 4: Adaptive Circuit Breaker

Dynamically adjust thresholds based on observed patterns.

```rust
struct AdaptiveBreaker {
    base_threshold: u32,
    current_threshold: AtomicU32,
    recent_history: Vec<StateTransition>,
}
```

**Pros:**
- Self-tuning for different failure patterns
- Can adapt to changing conditions

**Cons:**
- Complex to implement correctly
- Hard to predict behavior
- Difficult to debug
- Overkill for most use cases

---

## Decision

**Selected: Option 2 - Three-State Circuit Breaker**

### Rationale

1. **Proven pattern**: The three-state circuit breaker is a well-understood reliability pattern used in production systems (Netflix Hystrix, resilience4j, Polly).

2. **Gradual recovery**: The half-open state provides a controlled way to test if a backend has recovered without immediately flooding it with traffic.

3. **Predictable behavior**: Clear state machine makes it easy to understand and debug. Operators know exactly what state the breaker is in and why.

4. **Configurable**: Separate thresholds for failure and recovery allow tuning for different scenarios:
   - Low failure threshold for critical backends
   - High success threshold for backends that need sustained health

5. **Thread-safe implementation**: Atomic operations and minimal locking make it suitable for high-concurrency scenarios.

---

## Consequences

### Positive

- Failing backends are quickly removed from rotation
- Backends get recovery time without request pressure
- Gradual recovery prevents immediate re-failure
- Predictable state machine behavior
- Low overhead atomic implementation

### Negative

- Some requests fail immediately when circuit is open (fast-fail)
- Configuration requires understanding the pattern
- Single failure in half-open reopens circuit (intentionally conservative)

### Neutral

- Different from simple retry logic (complementary, not replacement)
- Requires monitoring to observe state transitions

---

## Implementation Details

### Default Configuration

```rust
pub const DEFAULT_FAILURE_THRESHOLD: u32 = 5;    // Failures to open
pub const DEFAULT_SUCCESS_THRESHOLD: u32 = 2;    // Successes to close
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);  // Open -> HalfOpen
```

### State Representation

Uses atomic u8 for lock-free state access:

```rust
#[repr(u8)]
pub enum CircuitBreakerState {
    Closed = 0,    // Normal operation
    Open = 1,      // Blocking requests
    HalfOpen = 2,  // Testing recovery
}

pub struct CircuitBreaker {
    state: AtomicU8,
    failure_count: AtomicU32,
    success_count: AtomicU32,
    opened_at: RwLock<Option<Instant>>,
}
```

### State Transitions

**Closed -> Open** (failure threshold reached):
```rust
fn record_failure(&self) {
    match self.state() {
        CircuitBreakerState::Closed => {
            let failures = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;
            if failures >= self.config.failure_threshold {
                self.transition_to_open();
            }
        }
        // ...
    }
}
```

**Open -> HalfOpen** (timeout elapsed):
```rust
fn state(&self) -> CircuitBreakerState {
    let current = CircuitBreakerState::from(self.state.load(Ordering::Acquire));

    if current == CircuitBreakerState::Open {
        if let Some(opened_at) = *self.opened_at.read() {
            if opened_at.elapsed() >= self.config.timeout {
                self.transition_to_half_open();
                return CircuitBreakerState::HalfOpen;
            }
        }
    }
    current
}
```

**HalfOpen -> Closed** (success threshold reached):
```rust
CircuitBreakerState::HalfOpen => {
    let successes = self.success_count.fetch_add(1, Ordering::AcqRel) + 1;
    if successes >= self.config.success_threshold {
        self.transition_to_closed();
    }
}
```

**HalfOpen -> Open** (any failure):
```rust
CircuitBreakerState::HalfOpen => {
    // Any failure in half-open immediately reopens
    self.transition_to_open();
}
```

### Integration with Load Balancer

The circuit breaker is checked before backend selection:

```rust
fn select_backend(&self) -> Option<&Backend> {
    let healthy: Vec<_> = self.backends.iter()
        .enumerate()
        .filter(|(_, b)| b.circuit_breaker.is_available())
        .map(|(i, _)| i)
        .collect();

    self.selector.select(&healthy)
        .map(|i| &self.backends[i])
}
```

### Success/Failure Recording

After each request:

```rust
match response_result {
    Ok(response) if response.status().is_success() => {
        backend.circuit_breaker.record_success();
    }
    Ok(response) if response.status().is_server_error() => {
        backend.circuit_breaker.record_failure();
    }
    Err(_) => {
        backend.circuit_breaker.record_failure();
    }
    _ => {
        // 4xx errors don't affect circuit (client errors)
    }
}
```

---

## Configuration Guidelines

| Scenario | Failure Threshold | Success Threshold | Timeout |
|----------|-------------------|-------------------|---------|
| **Default** | 5 | 2 | 30s |
| **Critical backend** | 3 | 3 | 60s |
| **Flaky but recoverable** | 10 | 1 | 15s |
| **Slow to recover** | 5 | 5 | 120s |

### Tuning Considerations

- **Lower failure threshold**: Opens circuit faster, but more sensitive to transient errors
- **Higher success threshold**: More confidence in recovery, but slower to restore traffic
- **Longer timeout**: More recovery time, but longer outage if backend is actually healthy

---

## Observability

Circuit breaker state should be exposed for monitoring:

```rust
// Metrics
circuit_breaker_state{backend="backend-1"} 0  // 0=Closed, 1=Open, 2=HalfOpen
circuit_breaker_failures_total{backend="backend-1"} 12
circuit_breaker_state_transitions_total{backend="backend-1",from="closed",to="open"} 3
```

---

## Alternatives Considered But Rejected

| Approach | Reason Not Selected |
|----------|---------------------|
| Simple failure counter | No gradual recovery; immediate re-disable on single failure |
| Sliding window rate | Higher complexity and memory; slower reaction to sudden failures |
| Adaptive thresholds | Unpredictable behavior; hard to debug; overkill |

---

## Related Decisions

- ADR-006: Load Balancer Strategy (circuit breaker integrates with backend selection)

---

## References

- [Circuit Breaker Pattern (Martin Fowler)](https://martinfowler.com/bliki/CircuitBreaker.html)
- [Release It! (Michael Nygard)](https://pragprog.com/titles/mnee2/release-it-second-edition/)
- [Netflix Hystrix](https://github.com/Netflix/Hystrix)
- [resilience4j Circuit Breaker](https://resilience4j.readme.io/docs/circuitbreaker)

---

## Changelog

| Date | Change |
|------|--------|
| 2025-12-30 | Initial decision documented |
