//! Criterion benchmarks for mikrozen-host runtime.
//!
//! Benchmarks:
//! - Circuit breaker overhead (check + record operations)
//! - Module cache operations (get/insert with moka)
//! - Script execution (JS runtime initialization)
//!
//! Run with:
//! ```bash
//! cargo bench -p mik --bench runtime_benchmarks
//! ```
//!
//! For HTML reports:
//! ```bash
//! cargo bench -p mik --bench runtime_benchmarks -- --verbose
//! open target/criterion/report/index.html
//! ```

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moka::sync::Cache as MokaCache;
use rquickjs::{Context as JsContext, Runtime as JsRuntime, Value as JsValue};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// =============================================================================
// Circuit Breaker Benchmarks
// =============================================================================

/// Minimal circuit breaker state for benchmarking overhead.
mod circuit_breaker {
    use moka::ops::compute::Op;
    use moka::sync::Cache as MokaCache;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[derive(Debug, Clone, Default)]
    pub enum CircuitState {
        #[default]
        Closed,
        Open {
            opened_at: Instant,
        },
        HalfOpen,
    }

    pub struct CircuitBreaker {
        states: MokaCache<Arc<str>, CircuitState>,
        timeout: Duration,
    }

    impl CircuitBreaker {
        pub fn new() -> Self {
            let states = MokaCache::builder()
                .max_capacity(1000)
                .time_to_idle(Duration::from_secs(600))
                .build();

            Self {
                states,
                timeout: Duration::from_secs(30),
            }
        }

        #[inline]
        pub fn check_request(&self, key: &str) -> bool {
            let cache_key: Arc<str> = Arc::from(key);
            let mut allowed = true;

            self.states
                .entry_by_ref(&cache_key)
                .and_compute_with(|entry| match entry {
                    Some(entry) => match entry.into_value() {
                        CircuitState::Closed => Op::Nop,
                        CircuitState::Open { opened_at } => {
                            if opened_at.elapsed() >= self.timeout {
                                Op::Put(CircuitState::HalfOpen)
                            } else {
                                allowed = false;
                                Op::Nop
                            }
                        },
                        CircuitState::HalfOpen => {
                            allowed = false;
                            Op::Nop
                        },
                    },
                    None => Op::Nop,
                });

            allowed
        }

        #[inline]
        pub fn record_success(&self, key: &str) {
            let cache_key: Arc<str> = Arc::from(key);
            self.states
                .entry_by_ref(&cache_key)
                .and_compute_with(|entry| match entry {
                    Some(_) => Op::Put(CircuitState::Closed),
                    None => Op::Nop,
                });
        }

        #[inline]
        pub fn record_failure(&self, key: &str) {
            let cache_key: Arc<str> = Arc::from(key);
            self.states.entry_by_ref(&cache_key).and_compute_with(|_| {
                Op::Put(CircuitState::Open {
                    opened_at: std::time::Instant::now(),
                })
            });
        }
    }
}

fn bench_circuit_breaker(c: &mut Criterion) {
    let mut group = c.benchmark_group("circuit_breaker");

    // Benchmark check_request (fast path - circuit closed/not tracked)
    group.bench_function("check_request_closed", |b| {
        let cb = circuit_breaker::CircuitBreaker::new();
        b.iter(|| {
            black_box(cb.check_request("service-1"));
        });
    });

    // Benchmark check + record success (typical request flow)
    group.bench_function("check_and_record_success", |b| {
        let cb = circuit_breaker::CircuitBreaker::new();
        // Pre-warm with a failure to ensure state exists
        cb.record_failure("service-1");
        cb.record_success("service-1");

        b.iter(|| {
            let allowed = cb.check_request("service-1");
            if allowed {
                cb.record_success("service-1");
            }
            black_box(allowed);
        });
    });

    // Benchmark with multiple keys (tests LRU cache behavior)
    for num_keys in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*num_keys as u64));
        group.bench_with_input(
            BenchmarkId::new("check_multiple_keys", num_keys),
            num_keys,
            |b, &num_keys| {
                let cb = circuit_breaker::CircuitBreaker::new();
                let keys: Vec<String> = (0..num_keys).map(|i| format!("service-{i}")).collect();

                b.iter(|| {
                    for key in &keys {
                        black_box(cb.check_request(key));
                    }
                });
            },
        );
    }

    group.finish();
}

// =============================================================================
// Module Cache Benchmarks
// =============================================================================

/// Simulated compiled component (just a size marker for benchmarking)
struct CachedComponent {
    size_bytes: usize,
    #[allow(dead_code)]
    data: Vec<u8>,
}

fn bench_module_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("module_cache");

    // Benchmark cache hit (warm cache)
    group.bench_function("cache_hit", |b| {
        let cache: MokaCache<String, Arc<CachedComponent>> = MokaCache::builder()
            .max_capacity(256 * 1024 * 1024) // 256MB
            .weigher(|_: &String, v: &Arc<CachedComponent>| {
                v.size_bytes.min(u32::MAX as usize) as u32
            })
            .build();

        // Pre-populate cache
        let component = Arc::new(CachedComponent {
            size_bytes: 100_000,
            data: vec![0u8; 100_000],
        });
        cache.insert("test-module".to_string(), component);

        b.iter(|| {
            black_box(cache.get("test-module"));
        });
    });

    // Benchmark cache miss
    group.bench_function("cache_miss", |b| {
        let cache: MokaCache<String, Arc<CachedComponent>> =
            MokaCache::builder().max_capacity(256 * 1024 * 1024).build();

        b.iter(|| {
            black_box(cache.get("nonexistent-module"));
        });
    });

    // Benchmark insert
    group.bench_function("cache_insert", |b| {
        let cache: MokaCache<String, Arc<CachedComponent>> = MokaCache::builder()
            .max_capacity(256 * 1024 * 1024)
            .weigher(|_: &String, v: &Arc<CachedComponent>| {
                v.size_bytes.min(u32::MAX as usize) as u32
            })
            .build();

        let counter = AtomicU64::new(0);

        b.iter(|| {
            let id = counter.fetch_add(1, Ordering::Relaxed);
            let component = Arc::new(CachedComponent {
                size_bytes: 100_000,
                data: vec![0u8; 100_000],
            });
            cache.insert(format!("module-{id}"), component);
        });
    });

    // Benchmark with LRU eviction pressure
    group.bench_function("cache_with_eviction", |b| {
        // Small cache to trigger eviction
        let cache: MokaCache<String, Arc<CachedComponent>> = MokaCache::builder()
            .max_capacity(1_000_000) // 1MB - will evict frequently
            .weigher(|_: &String, v: &Arc<CachedComponent>| {
                v.size_bytes.min(u32::MAX as usize) as u32
            })
            .build();

        let counter = AtomicU64::new(0);

        b.iter(|| {
            let id = counter.fetch_add(1, Ordering::Relaxed);
            let component = Arc::new(CachedComponent {
                size_bytes: 100_000, // 100KB - will cause eviction after ~10 inserts
                data: vec![0u8; 100_000],
            });
            cache.insert(format!("module-{id}"), component);
            // Also do a get to simulate real usage
            black_box(cache.get(&format!("module-{}", id.saturating_sub(5))));
        });
    });

    group.finish();
}

// =============================================================================
// Script Execution Benchmarks
// =============================================================================

fn bench_script_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("script_execution");

    // Benchmark JS runtime creation (cold start)
    group.bench_function("runtime_create", |b| {
        b.iter(|| {
            let runtime = JsRuntime::new().unwrap();
            let context = JsContext::full(&runtime).unwrap();
            black_box((runtime, context));
        });
    });

    // Benchmark simple script execution
    group.bench_function("eval_simple", |b| {
        let runtime = JsRuntime::new().unwrap();
        let context = JsContext::full(&runtime).unwrap();

        b.iter(|| {
            context.with(|ctx| {
                let result: JsValue = ctx.eval("1 + 1").unwrap();
                black_box(result);
            });
        });
    });

    // Benchmark JSON processing (common script pattern)
    group.bench_function("eval_json_transform", |b| {
        let runtime = JsRuntime::new().unwrap();
        let context = JsContext::full(&runtime).unwrap();

        // Set up input
        context.with(|ctx| {
            ctx.eval::<(), _>("var input = {value: 42, name: 'test'};")
                .unwrap();
        });

        let script = r"
            var output = {
                doubled: input.value * 2,
                greeting: 'Hello, ' + input.name
            };
            JSON.stringify(output);
        ";

        b.iter(|| {
            context.with(|ctx| {
                let result: JsValue = ctx.eval(script).unwrap();
                black_box(result);
            });
        });
    });

    // Benchmark with function call (mimics host.call pattern)
    group.bench_function("eval_function_call", |b| {
        let runtime = JsRuntime::new().unwrap();
        let context = JsContext::full(&runtime).unwrap();

        // Set up a mock function
        context.with(|ctx| {
            ctx.eval::<(), _>(r"
                var input = {items: [1, 2, 3, 4, 5]};
                function processItems(items) {
                    return items.map(function(x) { return x * 2; }).reduce(function(a, b) { return a + b; }, 0);
                }
            ").unwrap();
        });

        let script = "processItems(input.items);";

        b.iter(|| {
            context.with(|ctx| {
                let result: JsValue = ctx.eval(script).unwrap();
                black_box(result);
            });
        });
    });

    // Benchmark script preprocessing (export default transformation)
    group.bench_function("script_preprocess", |b| {
        let script = r"
            export default function(input) {
                var result = {};
                result.sum = input.items.reduce(function(a, b) { return a + b; }, 0);
                result.count = input.items.length;
                result.avg = result.sum / result.count;
                return result;
            }
        ";

        b.iter(|| {
            let processed = script
                .replace("export default function", "var __default__ = function")
                .replace(
                    "export default async function",
                    "var __default__ = async function",
                );
            let final_script = format!("{processed}\n__default__(input);");
            black_box(final_script);
        });
    });

    group.finish();
}

// =============================================================================
// Concurrent Access Benchmarks
// =============================================================================

fn bench_concurrent_access(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_access");

    // Configure for multi-threaded benchmarks
    group.sample_size(50);

    // Benchmark concurrent circuit breaker access
    group.bench_function("circuit_breaker_concurrent", |b| {
        let cb = Arc::new(circuit_breaker::CircuitBreaker::new());

        b.iter(|| {
            let handles: Vec<_> = (0..4)
                .map(|i| {
                    let cb = Arc::clone(&cb);
                    std::thread::spawn(move || {
                        for j in 0..100 {
                            let key = format!("service-{}-{}", i, j % 10);
                            cb.check_request(&key);
                            cb.record_success(&key);
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    // Benchmark concurrent cache access
    group.bench_function("module_cache_concurrent", |b| {
        let cache: Arc<MokaCache<String, Arc<CachedComponent>>> = Arc::new(
            MokaCache::builder()
                .max_capacity(100 * 1024 * 1024) // 100MB
                .weigher(|_: &String, v: &Arc<CachedComponent>| {
                    v.size_bytes.min(u32::MAX as usize) as u32
                })
                .build(),
        );

        // Pre-populate with some modules
        for i in 0..20 {
            cache.insert(
                format!("module-{i}"),
                Arc::new(CachedComponent {
                    size_bytes: 50_000,
                    data: vec![0u8; 50_000],
                }),
            );
        }

        b.iter(|| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let cache = Arc::clone(&cache);
                    std::thread::spawn(move || {
                        for j in 0..100 {
                            // Mix of hits and misses
                            let key = format!("module-{}", j % 30);
                            black_box(cache.get(&key));
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    group.finish();
}

// =============================================================================
// Criterion Configuration
// =============================================================================

criterion_group!(
    benches,
    bench_circuit_breaker,
    bench_module_cache,
    bench_script_execution,
    bench_concurrent_access,
);

criterion_main!(benches);
