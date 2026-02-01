// Benchmarks for order book operations
//
// Tests:
// - add_order latency
// - cancel_order latency
// - best_bid/best_ask lookup
// - Mixed workload (add/cancel/query)

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use exchange::order_book::OrderBook;
use common::Side;

/// Benchmark add_order latency
fn bench_add_order(c: &mut Criterion) {
    let mut group = c.benchmark_group("order_book_add");

    group.bench_function("add_single_order", |b| {
        let mut order_book = OrderBook::new(1);
        let mut order_id = 1u64;
        b.iter(|| {
            let result = order_book.add_order(
                black_box(100),        // client_id
                black_box(order_id),   // order_id
                black_box(Side::Buy),  // side
                black_box(10050),      // price
                black_box(100),        // qty
            );
            black_box(result);
            order_id += 1;
        });
    });

    // Benchmark with varying book depths
    for depth in [10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("add_to_book_depth", depth),
            depth,
            |b, &depth| {
                let mut order_book = OrderBook::new(1);
                // Pre-populate the book
                for i in 0..depth {
                    let _ = order_book.add_order(
                        100,
                        i as u64,
                        if i % 2 == 0 { Side::Buy } else { Side::Sell },
                        10000 + (i as i64),
                        100,
                    );
                }
                let mut order_id = depth as u64 + 1;
                b.iter(|| {
                    let result = order_book.add_order(
                        black_box(100),
                        black_box(order_id),
                        black_box(Side::Buy),
                        black_box(10050),
                        black_box(100),
                    );
                    black_box(result);
                    order_id += 1;
                });
            },
        );
    }

    // Benchmark adding to same price level (FIFO ordering)
    group.bench_function("add_same_price_level", |b| {
        let mut order_book = OrderBook::new(1);
        let mut order_id = 1u64;
        b.iter(|| {
            let result = order_book.add_order(
                black_box(100),
                black_box(order_id),
                black_box(Side::Buy),
                black_box(10000), // Same price every time
                black_box(100),
            );
            black_box(result);
            order_id += 1;
        });
    });

    // Benchmark adding to different price levels
    group.bench_function("add_different_price_levels", |b| {
        let mut order_book = OrderBook::new(1);
        let mut order_id = 1u64;
        let mut price = 10000i64;
        b.iter(|| {
            let result = order_book.add_order(
                black_box(100),
                black_box(order_id),
                black_box(Side::Buy),
                black_box(price),
                black_box(100),
            );
            black_box(result);
            order_id += 1;
            price += 1;
        });
    });

    group.finish();
}

/// Benchmark cancel_order latency
fn bench_cancel_order(c: &mut Criterion) {
    let mut group = c.benchmark_group("order_book_cancel");

    // Note: cancel_order currently returns None (not fully implemented)
    // but we still benchmark the lookup and map operations

    group.bench_function("cancel_nonexistent", |b| {
        let mut order_book = OrderBook::new(1);
        // Add some orders
        for i in 0..100 {
            let _ = order_book.add_order(100, i, Side::Buy, 10000 + (i as i64), 100);
        }
        let mut fake_id = 10000u64;
        b.iter(|| {
            let result = order_book.cancel_order(black_box(fake_id));
            black_box(result);
            fake_id += 1;
        });
    });

    // Benchmark with varying book sizes
    for book_size in [100, 1000, 10000].iter() {
        group.bench_with_input(
            BenchmarkId::new("cancel_lookup_in_book", book_size),
            book_size,
            |b, &size| {
                let mut order_book = OrderBook::new(1);
                // Pre-populate
                for i in 0..size {
                    let _ = order_book.add_order(
                        100,
                        i as u64,
                        if i % 2 == 0 { Side::Buy } else { Side::Sell },
                        10000 + (i as i64) % 100,
                        100,
                    );
                }
                let mut cancel_id = 0u64;
                b.iter(|| {
                    let result = order_book.cancel_order(black_box(cancel_id));
                    black_box(result);
                    cancel_id = (cancel_id + 1) % (size as u64);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark best_bid/best_ask lookup
fn bench_best_price_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("order_book_lookup");

    // Empty book lookups
    group.bench_function("best_bid_empty", |b| {
        let order_book = OrderBook::new(1);
        b.iter(|| {
            black_box(order_book.best_bid())
        });
    });

    group.bench_function("best_ask_empty", |b| {
        let order_book = OrderBook::new(1);
        b.iter(|| {
            black_box(order_book.best_ask())
        });
    });

    // Single order book lookups
    group.bench_function("best_bid_single", |b| {
        let mut order_book = OrderBook::new(1);
        let _ = order_book.add_order(100, 1, Side::Buy, 10000, 100);
        b.iter(|| {
            black_box(order_book.best_bid())
        });
    });

    group.bench_function("best_ask_single", |b| {
        let mut order_book = OrderBook::new(1);
        let _ = order_book.add_order(100, 1, Side::Sell, 10001, 100);
        b.iter(|| {
            black_box(order_book.best_ask())
        });
    });

    // Populated book lookups with varying depths
    for num_levels in [10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("best_bid_levels", num_levels),
            num_levels,
            |b, &levels| {
                let mut order_book = OrderBook::new(1);
                // Create bid levels
                for i in 0..levels {
                    let _ = order_book.add_order(
                        100,
                        i as u64,
                        Side::Buy,
                        10000 - (i as i64),
                        100,
                    );
                }
                b.iter(|| {
                    black_box(order_book.best_bid())
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("best_ask_levels", num_levels),
            num_levels,
            |b, &levels| {
                let mut order_book = OrderBook::new(1);
                // Create ask levels
                for i in 0..levels {
                    let _ = order_book.add_order(
                        100,
                        i as u64,
                        Side::Sell,
                        10001 + (i as i64),
                        100,
                    );
                }
                b.iter(|| {
                    black_box(order_book.best_ask())
                });
            },
        );
    }

    group.finish();
}

/// Benchmark mixed workload (add/cancel/query)
fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("order_book_mixed");

    // Workload: 70% add, 20% best_bid/ask, 10% cancel
    group.bench_function("mixed_70_20_10", |b| {
        let mut order_book = OrderBook::new(1);
        let mut order_id = 1u64;
        let mut iteration = 0u64;
        b.iter(|| {
            let op = iteration % 10;
            match op {
                0..=6 => {
                    // 70% add
                    let result = order_book.add_order(
                        black_box(100),
                        black_box(order_id),
                        black_box(if order_id % 2 == 0 { Side::Buy } else { Side::Sell }),
                        black_box(10000 + (order_id as i64 % 100)),
                        black_box(100),
                    );
                    black_box(result);
                    order_id += 1;
                }
                7 | 8 => {
                    // 20% lookup
                    if iteration % 2 == 0 {
                        black_box(order_book.best_bid());
                    } else {
                        black_box(order_book.best_ask());
                    }
                }
                _ => {
                    // 10% cancel (will mostly fail since cancel isn't fully implemented)
                    let cancel_id = if order_id > 10 { order_id - 10 } else { 0 };
                    black_box(order_book.cancel_order(black_box(cancel_id)));
                }
            }
            iteration += 1;
        });
    });

    // High frequency order flow simulation
    group.bench_function("high_frequency_add_lookup", |b| {
        let mut order_book = OrderBook::new(1);
        let mut order_id = 1u64;
        b.iter(|| {
            // Add order
            let _ = order_book.add_order(
                black_box(100),
                black_box(order_id),
                black_box(Side::Buy),
                black_box(10000),
                black_box(100),
            );
            // Immediate lookup
            black_box(order_book.best_bid());
            order_id += 1;
        });
    });

    group.finish();
}

/// Benchmark order book statistics queries
fn bench_statistics(c: &mut Criterion) {
    let mut group = c.benchmark_group("order_book_stats");

    for book_size in [100, 1000, 10000].iter() {
        group.bench_with_input(
            BenchmarkId::new("order_count", book_size),
            book_size,
            |b, &size| {
                let mut order_book = OrderBook::new(1);
                for i in 0..size {
                    let _ = order_book.add_order(
                        100,
                        i as u64,
                        if i % 2 == 0 { Side::Buy } else { Side::Sell },
                        10000 + (i as i64 % 100),
                        100,
                    );
                }
                b.iter(|| {
                    black_box(order_book.order_count())
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("bid_level_count", book_size),
            book_size,
            |b, &size| {
                let mut order_book = OrderBook::new(1);
                for i in 0..size {
                    let _ = order_book.add_order(
                        100,
                        i as u64,
                        Side::Buy,
                        10000 + (i as i64),
                        100,
                    );
                }
                b.iter(|| {
                    black_box(order_book.bid_level_count())
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("ask_level_count", book_size),
            book_size,
            |b, &size| {
                let mut order_book = OrderBook::new(1);
                for i in 0..size {
                    let _ = order_book.add_order(
                        100,
                        i as u64,
                        Side::Sell,
                        10001 + (i as i64),
                        100,
                    );
                }
                b.iter(|| {
                    black_box(order_book.ask_level_count())
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_add_order,
    bench_cancel_order,
    bench_best_price_lookup,
    bench_mixed_workload,
    bench_statistics,
);

criterion_main!(benches);
