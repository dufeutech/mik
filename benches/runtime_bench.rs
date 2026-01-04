//! Criterion benchmarks for the high-performance runtime.
//!
//! These benchmarks measure the performance of critical runtime components
//! to track regressions and validate optimizations.
//!
//! # Benchmark Categories
//!
//! - **Buffer Pool**: Acquire/release operations, pool exhaustion
//! - **Store Pool**: Wasmtime store pooling performance
//! - **Scheduling**: Round-robin and worker selection overhead
//! - **Metrics**: Counter and histogram update overhead
//!
//! # Running Benchmarks
//!
//! ```bash
//! # Run all benchmarks
//! cargo bench --bench runtime_bench
//!
//! # Run specific benchmark group
//! cargo bench --bench runtime_bench -- buffer_pool
//!
//! # Generate HTML report
//! cargo bench --bench runtime_bench -- --verbose
//! open target/criterion/report/index.html
//! ```
//!
//! # Expected Performance
//!
//! | Operation              | Target Latency |
//! |------------------------|----------------|
//! | Buffer acquire/release | < 50ns         |
//! | Store acquire/release  | < 1us          |
//! | Round-robin next()     | < 10ns         |
//! | Counter increment      | < 5ns          |

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

// =============================================================================
// Buffer Pool Benchmarks
// =============================================================================

/// Buffer pool implementation for benchmarking.
mod buffers {
    use std::sync::Mutex;

    pub struct BufferPool {
        pool: Mutex<Vec<Vec<u8>>>,
        buffer_size: usize,
        max_capacity: usize,
    }

    impl BufferPool {
        pub fn new(initial_count: usize, buffer_size: usize, max_capacity: usize) -> Self {
            let mut pool = Vec::with_capacity(max_capacity);
            for _ in 0..initial_count.min(max_capacity) {
                pool.push(vec![0u8; buffer_size]);
            }
            Self {
                pool: Mutex::new(pool),
                buffer_size,
                max_capacity,
            }
        }

        #[inline]
        pub fn acquire(&self) -> Vec<u8> {
            self.pool
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| vec![0u8; self.buffer_size])
        }

        #[inline]
        pub fn release(&self, mut buf: Vec<u8>) {
            buf.clear();
            let mut pool = self.pool.lock().unwrap();
            if pool.len() < self.max_capacity {
                pool.push(buf);
            }
        }
    }
}

fn buffer_pool_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool");

    // Benchmark single acquire/release cycle (hot path)
    group.bench_function("acquire_release_single", |b| {
        let pool = buffers::BufferPool::new(100, 8192, 128);

        b.iter(|| {
            let buf = pool.acquire();
            pool.release(buf);
        });
    });

    // Benchmark with different pool sizes
    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("acquire_release_batch", size),
            size,
            |b, &size| {
                let pool = buffers::BufferPool::new(size, 8192, size * 2);
                let mut buffers = Vec::with_capacity(size);

                b.iter(|| {
                    // Acquire all
                    for _ in 0..size {
                        buffers.push(pool.acquire());
                    }
                    // Release all
                    while let Some(buf) = buffers.pop() {
                        pool.release(buf);
                    }
                });
            },
        );
    }

    // Benchmark pool exhaustion (worst case - must allocate)
    group.bench_function("acquire_exhausted", |b| {
        let pool = buffers::BufferPool::new(0, 8192, 10); // Empty pool

        b.iter(|| {
            let buf = pool.acquire();
            black_box(buf);
            // Don't release - simulates exhaustion
        });
    });

    // Benchmark with different buffer sizes
    for buffer_size in [1024, 8192, 65536].iter() {
        group.bench_with_input(
            BenchmarkId::new("acquire_release_size", buffer_size),
            buffer_size,
            |b, &buffer_size| {
                let pool = buffers::BufferPool::new(50, buffer_size, 100);

                b.iter(|| {
                    let buf = pool.acquire();
                    pool.release(buf);
                });
            },
        );
    }

    group.finish();
}

// =============================================================================
// Store Pool Benchmarks
// =============================================================================

/// Pool pattern benchmarks (baseline for object pooling overhead).
fn store_pool_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_pool");

    // Baseline benchmark - measures overhead of a simple pool pattern
    group.bench_function("pool_acquire_release", |b| {
        let pool = buffers::BufferPool::new(100, 1024, 200);

        b.iter(|| {
            let item = pool.acquire();
            black_box(&item);
            pool.release(item);
        });
    });

    group.finish();
}

// =============================================================================
// Scheduling Benchmarks
// =============================================================================

/// Round-robin scheduler for benchmarking.
mod scheduling {
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub struct RoundRobin {
        num_workers: usize,
        next: AtomicUsize,
    }

    impl RoundRobin {
        pub fn new(num_workers: usize) -> Self {
            Self {
                num_workers,
                next: AtomicUsize::new(0),
            }
        }

        #[inline]
        pub fn next(&self) -> usize {
            // Use fetch_add with modulo for atomic round-robin
            let n = self.next.fetch_add(1, Ordering::Relaxed);
            n % self.num_workers
        }
    }
}

fn scheduling_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("scheduling");

    // Benchmark round-robin selection
    group.bench_function("round_robin_next", |b| {
        let scheduler = scheduling::RoundRobin::new(8);

        b.iter(|| {
            black_box(scheduler.next());
        });
    });

    // Benchmark with different worker counts
    for num_workers in [2, 4, 8, 16, 32].iter() {
        group.bench_with_input(
            BenchmarkId::new("round_robin_workers", num_workers),
            num_workers,
            |b, &num_workers| {
                let scheduler = scheduling::RoundRobin::new(num_workers);

                b.iter(|| {
                    black_box(scheduler.next());
                });
            },
        );
    }

    group.finish();
}

// =============================================================================
// Metrics Benchmarks
// =============================================================================

/// Metrics implementation for benchmarking.
mod metrics {
    use std::sync::atomic::{AtomicU64, Ordering};

    pub struct Metrics {
        pub requests_total: AtomicU64,
        pub requests_in_flight: AtomicU64,
        pub bytes_received: AtomicU64,
        pub bytes_sent: AtomicU64,
        pub status_2xx: AtomicU64,
        pub status_4xx: AtomicU64,
        pub status_5xx: AtomicU64,
    }

    impl Metrics {
        pub fn new() -> Self {
            Self {
                requests_total: AtomicU64::new(0),
                requests_in_flight: AtomicU64::new(0),
                bytes_received: AtomicU64::new(0),
                bytes_sent: AtomicU64::new(0),
                status_2xx: AtomicU64::new(0),
                status_4xx: AtomicU64::new(0),
                status_5xx: AtomicU64::new(0),
            }
        }

        #[inline]
        pub fn inc_requests(&self) {
            self.requests_total.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn request_started(&self) {
            self.requests_in_flight.fetch_add(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn request_finished(&self) {
            self.requests_in_flight.fetch_sub(1, Ordering::Relaxed);
        }

        #[inline]
        pub fn inc_status(&self, status: u16) {
            match status {
                200..=299 => self.status_2xx.fetch_add(1, Ordering::Relaxed),
                400..=499 => self.status_4xx.fetch_add(1, Ordering::Relaxed),
                500..=599 => self.status_5xx.fetch_add(1, Ordering::Relaxed),
                _ => 0,
            };
        }

        #[inline]
        pub fn add_bytes(&self, received: u64, sent: u64) {
            self.bytes_received.fetch_add(received, Ordering::Relaxed);
            self.bytes_sent.fetch_add(sent, Ordering::Relaxed);
        }
    }
}

fn metrics_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("metrics");

    // Benchmark counter increment (most common operation)
    group.bench_function("counter_increment", |b| {
        let metrics = metrics::Metrics::new();

        b.iter(|| {
            metrics.inc_requests();
        });
    });

    // Benchmark request start/finish cycle
    group.bench_function("request_lifecycle", |b| {
        let metrics = metrics::Metrics::new();

        b.iter(|| {
            metrics.request_started();
            metrics.inc_requests();
            metrics.inc_status(200);
            metrics.add_bytes(1024, 2048);
            metrics.request_finished();
        });
    });

    // Benchmark concurrent counter updates (simulates multi-worker)
    group.sample_size(50);
    group.bench_function("counter_concurrent", |b| {
        use std::sync::Arc;

        let metrics = Arc::new(metrics::Metrics::new());

        b.iter(|| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let m = Arc::clone(&metrics);
                    std::thread::spawn(move || {
                        for _ in 0..100 {
                            m.inc_requests();
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
// Concurrent Access Benchmarks
// =============================================================================

fn concurrent_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent");
    group.sample_size(50);

    // Benchmark concurrent buffer pool access
    group.bench_function("buffer_pool_contention", |b| {
        use std::sync::Arc;

        let pool = Arc::new(buffers::BufferPool::new(100, 8192, 200));

        b.iter(|| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let p = Arc::clone(&pool);
                    std::thread::spawn(move || {
                        for _ in 0..50 {
                            let buf = p.acquire();
                            black_box(&buf);
                            p.release(buf);
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    // Benchmark mixed workload (read-heavy)
    group.bench_function("mixed_workload_read_heavy", |b| {
        use std::sync::Arc;

        let pool = Arc::new(buffers::BufferPool::new(100, 8192, 200));
        let metrics = Arc::new(metrics::Metrics::new());

        b.iter(|| {
            let handles: Vec<_> = (0..4)
                .map(|i| {
                    let p = Arc::clone(&pool);
                    let m = Arc::clone(&metrics);
                    std::thread::spawn(move || {
                        for j in 0..25 {
                            // 80% reads (metrics), 20% writes (buffer)
                            if (i + j) % 5 != 0 {
                                m.inc_requests();
                            } else {
                                let buf = p.acquire();
                                black_box(&buf);
                                p.release(buf);
                            }
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
    buffer_pool_benchmark,
    store_pool_benchmark,
    scheduling_benchmark,
    metrics_benchmark,
    concurrent_benchmark,
);

criterion_main!(benches);
