// Benchmarks for lock-free SPSC queue
//
// Tests:
// - Push/pop throughput
// - Burst patterns
// - Single vs batched operations

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput, BenchmarkId};
use common::lf_queue::LFQueue;

/// Benchmark single push/pop operations
fn bench_push_pop_single(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_single_ops");
    group.throughput(Throughput::Elements(1));

    group.bench_function("push", |b| {
        let queue: LFQueue<u64, 1024> = LFQueue::new();
        let mut counter = 0u64;
        b.iter(|| {
            // Push one item
            let _ = queue.push(black_box(counter));
            counter = counter.wrapping_add(1);
            // Pop to make room for next iteration
            let _ = queue.pop();
        });
    });

    group.bench_function("pop", |b| {
        let queue: LFQueue<u64, 1024> = LFQueue::new();
        // Pre-fill with one item
        let _ = queue.push(42);
        b.iter(|| {
            // Pop the item
            let item = queue.pop();
            black_box(item);
            // Push to refill for next iteration
            let _ = queue.push(42);
        });
    });

    group.bench_function("push_pop_roundtrip", |b| {
        let queue: LFQueue<u64, 1024> = LFQueue::new();
        let mut counter = 0u64;
        b.iter(|| {
            let _ = queue.push(black_box(counter));
            counter = counter.wrapping_add(1);
            let item = queue.pop();
            black_box(item)
        });
    });

    group.finish();
}

/// Benchmark throughput with varying queue sizes
fn bench_queue_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_throughput");

    for batch_size in [16, 64, 256, 1024].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));

        group.bench_with_input(
            BenchmarkId::new("push_batch", batch_size),
            batch_size,
            |b, &size| {
                let queue: LFQueue<u64, 4096> = LFQueue::new();
                b.iter(|| {
                    // Push batch
                    for i in 0..size {
                        let _ = queue.push(black_box(i as u64));
                    }
                    // Pop batch to reset
                    for _ in 0..size {
                        black_box(queue.pop());
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("pop_batch", batch_size),
            batch_size,
            |b, &size| {
                let queue: LFQueue<u64, 4096> = LFQueue::new();
                // Pre-fill
                for i in 0..size {
                    let _ = queue.push(i as u64);
                }
                b.iter(|| {
                    // Pop batch
                    for _ in 0..size {
                        black_box(queue.pop());
                    }
                    // Refill for next iteration
                    for i in 0..size {
                        let _ = queue.push(i as u64);
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark burst patterns (simulate producer bursts)
fn bench_burst_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_burst");

    // Simulate a burst of N items followed by consumption
    for burst_size in [8, 32, 128].iter() {
        group.throughput(Throughput::Elements(*burst_size as u64));

        group.bench_with_input(
            BenchmarkId::new("burst_then_drain", burst_size),
            burst_size,
            |b, &size| {
                let queue: LFQueue<u64, 4096> = LFQueue::new();
                b.iter(|| {
                    // Burst push
                    for i in 0..size {
                        let _ = queue.push(black_box(i as u64));
                    }
                    // Drain
                    while let Some(item) = queue.pop() {
                        black_box(item);
                    }
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("alternating_burst", burst_size),
            burst_size,
            |b, &size| {
                let queue: LFQueue<u64, 4096> = LFQueue::new();
                b.iter(|| {
                    // Push half
                    for i in 0..(size / 2) {
                        let _ = queue.push(black_box(i as u64));
                    }
                    // Pop some
                    for _ in 0..(size / 4) {
                        black_box(queue.pop());
                    }
                    // Push remaining
                    for i in (size / 2)..size {
                        let _ = queue.push(black_box(i as u64));
                    }
                    // Drain remaining
                    while let Some(item) = queue.pop() {
                        black_box(item);
                    }
                });
            },
        );
    }

    group.finish();
}

/// Compare single operations vs batched
fn bench_single_vs_batched(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_single_vs_batched");
    let total_ops = 256;
    group.throughput(Throughput::Elements(total_ops as u64));

    group.bench_function("single_ops", |b| {
        let queue: LFQueue<u64, 4096> = LFQueue::new();
        b.iter(|| {
            for i in 0..total_ops {
                let _ = queue.push(black_box(i as u64));
                black_box(queue.pop());
            }
        });
    });

    group.bench_function("batched_push_then_pop", |b| {
        let queue: LFQueue<u64, 4096> = LFQueue::new();
        b.iter(|| {
            // Batch push
            for i in 0..total_ops {
                let _ = queue.push(black_box(i as u64));
            }
            // Batch pop
            for _ in 0..total_ops {
                black_box(queue.pop());
            }
        });
    });

    group.finish();
}

/// Benchmark queue with different element sizes
fn bench_element_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_element_sizes");
    let ops = 64;
    group.throughput(Throughput::Elements(ops as u64));

    group.bench_function("u64", |b| {
        let queue: LFQueue<u64, 1024> = LFQueue::new();
        b.iter(|| {
            for i in 0..ops {
                let _ = queue.push(black_box(i as u64));
            }
            for _ in 0..ops {
                black_box(queue.pop());
            }
        });
    });

    group.bench_function("u128", |b| {
        let queue: LFQueue<u128, 1024> = LFQueue::new();
        b.iter(|| {
            for i in 0..ops {
                let _ = queue.push(black_box(i as u128));
            }
            for _ in 0..ops {
                black_box(queue.pop());
            }
        });
    });

    // Test with a larger struct similar to order data
    #[derive(Clone, Copy)]
    struct OrderData {
        order_id: u64,
        client_id: u32,
        ticker_id: u32,
        price: i64,
        qty: u32,
        side: i8,
        _padding: [u8; 7],
    }

    group.bench_function("order_struct_40bytes", |b| {
        let queue: LFQueue<OrderData, 1024> = LFQueue::new();
        let order = OrderData {
            order_id: 12345,
            client_id: 100,
            ticker_id: 1,
            price: 10050,
            qty: 100,
            side: 1,
            _padding: [0; 7],
        };
        b.iter(|| {
            for _ in 0..ops {
                let _ = queue.push(black_box(order));
            }
            for _ in 0..ops {
                black_box(queue.pop());
            }
        });
    });

    group.finish();
}

/// Benchmark empty queue pop (fast path)
fn bench_empty_pop(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_empty");

    group.bench_function("pop_empty", |b| {
        let queue: LFQueue<u64, 1024> = LFQueue::new();
        b.iter(|| {
            black_box(queue.pop())
        });
    });

    group.bench_function("is_empty_check", |b| {
        let queue: LFQueue<u64, 1024> = LFQueue::new();
        b.iter(|| {
            black_box(queue.is_empty())
        });
    });

    group.bench_function("len_check", |b| {
        let queue: LFQueue<u64, 1024> = LFQueue::new();
        b.iter(|| {
            black_box(queue.len())
        });
    });

    group.finish();
}

/// Benchmark full queue push (rejection path)
fn bench_full_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_full");

    group.bench_function("push_full", |b| {
        let queue: LFQueue<u64, 64> = LFQueue::new();
        // Fill the queue
        for i in 0..64 {
            let _ = queue.push(i);
        }
        b.iter(|| {
            black_box(queue.push(black_box(999)))
        });
    });

    group.bench_function("is_full_check", |b| {
        let queue: LFQueue<u64, 64> = LFQueue::new();
        // Fill the queue
        for i in 0..64 {
            let _ = queue.push(i);
        }
        b.iter(|| {
            black_box(queue.is_full())
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_push_pop_single,
    bench_queue_throughput,
    bench_burst_patterns,
    bench_single_vs_batched,
    bench_element_sizes,
    bench_empty_pop,
    bench_full_push,
);

criterion_main!(benches);
