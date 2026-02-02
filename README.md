# Rust Low-Latency Trading System

A complete low-latency electronic trading system written in Rust, implementing the concepts and patterns from **"Building Low Latency Applications with C++"** by Sourav Ghosh.

This project demonstrates how to build a production-quality trading infrastructure with sub-microsecond latency characteristics, translating proven C++ low-latency patterns into idiomatic, memory-safe Rust.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        TRADING SYSTEM ARCHITECTURE                          │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────────┐          TCP           ┌─────────────────────────────┐ │
│  │  Trading Client │◄────────────────────►  │       Exchange Server       │ │
│  │                 │    ClientRequest/      │                             │ │
│  │  • Market Maker │    ClientResponse      │  • Order Server (TCP)       │ │
│  │  • Liquidity    │                        │  • Matching Engine          │ │
│  │    Taker        │                        │  • Order Books              │ │
│  │  • Risk Manager │                        │  • Market Data Publisher    │ │
│  │  • Position     │                        │                             │ │
│  │    Keeper       │◄───────────────────────│                             │ │
│  └─────────────────┘    Multicast UDP       └─────────────────────────────┘ │
│                         MarketUpdate                                        │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Features

- **Lock-Free Data Structures**: SPSC queues and memory pools with cache-line alignment
- **Zero-Copy Protocols**: Binary message formats with `#[repr(C, packed)]`
- **Pre-Allocated Memory**: No allocations in the hot path after initialization
- **Non-Blocking I/O**: Polling-based TCP and multicast UDP networking
- **Complete Trading Stack**: Order matching, market data, risk management, strategies
- **Professional Risk Controls**: Position limits, P&L limits, order size limits

---

## Project Structure

```
rust-low-latency-trading/
├── Cargo.toml              # Workspace configuration
├── common/                 # Shared low-latency primitives
│   ├── src/
│   │   ├── lib.rs          # Public API exports
│   │   ├── types.rs        # Core type definitions (OrderId, Price, Qty, Side)
│   │   ├── lf_queue.rs     # Lock-free SPSC queue
│   │   ├── mem_pool.rs     # Pre-allocated memory pool
│   │   ├── time.rs         # Nanosecond timing, RDTSC support
│   │   ├── logging.rs      # Lock-free async logger
│   │   └── net/            # Non-blocking TCP and multicast sockets
│   └── benches/
│       └── queue_bench.rs  # SPSC queue benchmarks
│
├── exchange/               # Exchange server components
│   ├── src/
│   │   ├── main.rs         # Server binary entry point
│   │   ├── protocol.rs     # Binary message definitions
│   │   ├── order_book.rs   # Price-time priority order book
│   │   ├── matching_engine.rs
│   │   ├── order_server.rs # TCP gateway with FIFO sequencing
│   │   └── market_data.rs  # Multicast publisher
│   └── benches/
│       ├── order_book_bench.rs
│       └── e2e_latency.rs
│
└── trading/                # Trading client components
    ├── src/
    │   ├── main.rs         # Client binary entry point
    │   ├── market_data.rs  # Multicast subscriber
    │   ├── order_gateway.rs # TCP order submission
    │   ├── features.rs     # Signal generation (fair value, imbalance)
    │   ├── position.rs     # Position and P&L tracking
    │   ├── risk.rs         # Pre-trade risk validation
    │   ├── trade_engine.rs # Order execution coordinator
    │   └── strategies/
    │       ├── market_maker.rs     # Quote-based liquidity provision
    │       └── liquidity_taker.rs  # Signal-based aggressive execution
    └── tests/
        └── integration.rs  # End-to-end tests
```

---

## Code Showcases

### 1. Cache-Line Aligned Lock-Free Queue

The SPSC queue prevents false sharing by placing producer and consumer indices on separate cache lines:

```rust
/// Cache-line aligned writer index.
/// Separated from reader index to prevent false sharing.
#[repr(align(64))]
struct WriterIndex {
    tail: AtomicUsize,
}

/// Cache-line aligned reader index.
#[repr(align(64))]
struct ReaderIndex {
    head: AtomicUsize,
}

pub struct LFQueue<T, const N: usize> {
    buffer: UnsafeCell<[MaybeUninit<T>; N]>,
    writer: WriterIndex,
    reader: ReaderIndex,
}
```

The push operation uses careful memory ordering:

```rust
pub fn push(&self, item: T) -> Result<(), T> {
    let tail = self.writer.tail.load(Ordering::Relaxed);
    let head = self.reader.head.load(Ordering::Acquire);

    if tail.wrapping_sub(head) >= N {
        return Err(item);  // Queue full
    }

    unsafe {
        let buffer = &mut *self.buffer.get();
        buffer[tail & Self::MASK].write(item);
    }

    // Release ensures the write is visible before updating tail
    self.writer.tail.store(tail.wrapping_add(1), Ordering::Release);
    Ok(())
}
```

### 2. Zero-Allocation Memory Pool

Pre-allocated pool with stack-based free list for O(1) allocation:

```rust
pub struct MemPool<T, const N: usize> {
    storage: UnsafeCell<[MaybeUninit<T>; N]>,
    free_list: UnsafeCell<[usize; N]>,  // Stack of free indices
    free_count: UnsafeCell<usize>,
}

#[inline]
pub fn allocate(&self) -> Option<PoolPtr<T>> {
    unsafe {
        let free_count = &mut *self.free_count.get();
        if *free_count == 0 {
            return None;
        }
        *free_count -= 1;
        let index = (*self.free_list.get())[*free_count];
        let ptr = (*self.storage.get())[index].as_mut_ptr();
        Some(PoolPtr { index, ptr, _marker: PhantomData })
    }
}
```

`PoolPtr` doesn't implement `Clone`, preventing double-free at compile time:

```rust
pub struct PoolPtr<T> {
    index: usize,
    ptr: *mut T,
    _marker: PhantomData<T>,
}
// Note: No Clone implementation - ownership transfer only
```

### 3. Zero-Copy Binary Protocol

Messages use packed representation for direct transmission:

```rust
#[repr(C, packed)]
#[derive(AsBytes, FromBytes, FromZeroes)]
pub struct ClientRequest {
    pub msg_type: u8,      // 1 byte
    pub client_id: u32,    // 4 bytes
    pub ticker_id: u32,    // 4 bytes
    pub order_id: u64,     // 8 bytes
    pub side: i8,          // 1 byte
    pub price: i64,        // 8 bytes (cents)
    pub qty: u32,          // 4 bytes
}  // 30 bytes total

// Zero-copy send - no serialization overhead
let bytes = request.as_bytes();
socket.send(bytes)?;
```

### 4. Order Book with Price-Time Priority

Index-based doubly-linked list within memory pool:

```rust
pub struct OrderBook {
    ticker_id: TickerId,
    bid_levels: HashMap<Price, PriceLevel>,
    ask_levels: HashMap<Price, PriceLevel>,
    order_map: HashMap<OrderId, OrderIndex>,  // O(1) lookup
    order_pool: Box<MemPool<Order, 65536>>,   // Pre-allocated
    next_priority: Priority,
}

pub struct Order {
    order_id: OrderId,
    side: Side,
    price: Price,
    qty: Qty,
    priority: Priority,
    prev_idx: Option<usize>,  // Linked list via indices
    next_idx: Option<usize>,
}
```

### 5. Lock-Free Async Logger

Deferred formatting keeps the hot path allocation-free:

```rust
pub enum LogMessage {
    Static(&'static str),                    // Zero allocation
    StaticWithI64(&'static str, i64),       // Deferred formatting
    StaticWithU64(&'static str, u64),
    StaticWithF64(&'static str, f64),
    Formatted(String),                       // Only when unavoidable
}

#[inline]
pub fn log(&self, level: LogLevel, msg: &'static str) {
    if level < self.min_level { return; }

    let entry = LogEntry {
        timestamp: now_nanos(),
        level,
        message: LogMessage::Static(msg),  // No allocation!
    };
    let _ = self.shared.queue.push(entry);  // Non-blocking
}
```

Background thread with progressive backoff:

```rust
fn writer_loop(shared: Arc<LoggerShared>) {
    let mut idle_count = 0u32;

    while shared.running.load(Ordering::Relaxed) {
        while let Some(entry) = shared.queue.pop() {
            Self::write_entry(&entry);
            idle_count = 0;
        }

        // Progressive backoff: spin → yield → sleep
        if idle_count < 100 {
            std::hint::spin_loop();
        } else if idle_count < 1100 {
            thread::yield_now();
        } else {
            thread::sleep(Duration::from_micros(100));
        }
        idle_count = idle_count.saturating_add(1);
    }
}
```

### 6. Market Maker Strategy with Position Skew

Inventory-aware quoting adjusts for position risk:

```rust
pub fn generate_quotes(&self, features: &TickerFeatures, position: i64) -> QuotePair {
    let fair_value = features.fair_value;

    // Widen spread on imbalanced books
    let imbalance_adj = features.imbalance.abs() * self.config.half_spread as f64 * 0.5;
    let half_spread = self.config.half_spread + imbalance_adj as i64;

    // Skew quotes away from inventory
    let position_ratio = (position as f64 / self.config.max_position as f64).clamp(-1.0, 1.0);
    let bid_factor = 1.0 - (self.config.position_skew_factor * position_ratio).max(0.0);
    let ask_factor = 1.0 + (self.config.position_skew_factor * position_ratio).min(0.0);

    QuotePair {
        bid_price: fair_value - half_spread,
        bid_qty: (self.config.base_qty as f64 * bid_factor) as Qty,
        ask_price: fair_value + half_spread,
        ask_qty: (self.config.base_qty as f64 * ask_factor) as Qty,
    }
}
```

### 7. Feature Engine with EMA Smoothing

```rust
pub fn on_bbo_update(&mut self, ticker_id: TickerId, bbo: &BBO) {
    let mid = (bbo.bid_price + bbo.ask_price) / 2;
    let spread = bbo.ask_price - bbo.bid_price;

    // EMA: fair_value = alpha * mid + (1 - alpha) * prev
    let prev_fv = self.features.get(&ticker_id).map(|f| f.fair_value).unwrap_or(mid);
    let fair_value = ((self.alpha * mid as f64) + ((1.0 - self.alpha) * prev_fv as f64)) as Price;

    // Normalized order book imbalance: -1.0 to +1.0
    let total_qty = bbo.bid_qty + bbo.ask_qty;
    let imbalance = if total_qty > 0 {
        (bbo.bid_qty as f64 - bbo.ask_qty as f64) / total_qty as f64
    } else { 0.0 };

    // Trade signal combining mean reversion and momentum
    let fv_signal = (fair_value - mid) as f64 / spread as f64;
    let trade_signal = (0.7 * fv_signal + 0.3 * imbalance).clamp(-1.0, 1.0);

    self.features.insert(ticker_id, TickerFeatures {
        fair_value, spread, mid_price: mid, imbalance, trade_signal
    });
}
```

### 8. Multi-Layer Risk Management

```rust
pub fn check_order(&self, ticker: TickerId, side: Side, qty: Qty,
                   position: &Position, open_orders: usize) -> RiskCheckResult {
    // Layer 1: Order size
    if qty > self.limits.max_order_qty {
        return RiskCheckResult::Rejected(RiskRejection::OrderTooLarge);
    }

    // Layer 2: Position limits (skip for risk-reducing trades)
    let projected = match side {
        Side::Buy => position.position + position.open_buy_qty as i64 + qty as i64,
        Side::Sell => position.position - position.open_sell_qty as i64 - qty as i64,
    };
    let is_risk_reducing = (side == Side::Buy && position.position < 0)
                        || (side == Side::Sell && position.position > 0);

    if !is_risk_reducing && projected.abs() > self.limits.max_position as i64 {
        return RiskCheckResult::Rejected(RiskRejection::PositionTooLarge);
    }

    // Layer 3: P&L check
    let total_pnl = position.realized_pnl + position.unrealized_pnl;
    if total_pnl < -self.limits.max_loss {
        return RiskCheckResult::Rejected(RiskRejection::LossTooLarge);
    }

    // Layer 4: Open order count
    if open_orders >= self.limits.max_open_orders {
        return RiskCheckResult::Rejected(RiskRejection::OpenOrdersTooMany);
    }

    RiskCheckResult::Allowed
}
```

---

## Performance Characteristics

| Component | Complexity | Latency Target |
|-----------|------------|----------------|
| SPSC Queue Push/Pop | O(1) | < 100ns |
| Memory Pool Allocate/Free | O(1) | < 50ns |
| Order Book Add | O(1) amortized | < 500ns |
| Order Book Cancel | O(1) | < 200ns |
| Message Serialization | O(1) | 0ns (zero-copy) |
| Feature Calculation | O(1) | < 100ns |
| Risk Check | O(1) | < 100ns |

---

## Quick Start

### Build

```bash
cargo build --release
```

### Run Exchange Server

```bash
./target/release/exchange \
    --port 12345 \
    --multicast-addr 239.255.0.1 \
    --multicast-port 5000 \
    --tickers 1,2,3
```

### Run Trading Client (Market Maker)

```bash
./target/release/trading \
    --host 127.0.0.1 \
    --port 12345 \
    --strategy market-maker \
    --ticker 1 \
    --half-spread 50 \
    --max-position 1000 \
    --max-loss 100000
```

### Run Trading Client (Liquidity Taker)

```bash
./target/release/trading \
    --host 127.0.0.1 \
    --port 12345 \
    --strategy liquidity-taker \
    --ticker 1 \
    --signal-threshold 0.3
```

### Run Benchmarks

```bash
# SPSC queue benchmarks
cargo bench --package common --bench queue_bench

# Order book benchmarks
cargo bench --package exchange --bench order_book_bench

# End-to-end latency
cargo bench --package exchange --bench e2e_latency
```

### Run Tests

```bash
cargo test --workspace
```

---

## Configuration Reference

### Exchange Server

| Flag | Default | Description |
|------|---------|-------------|
| `--port, -p` | 12345 | TCP listen port |
| `--multicast-addr, -m` | 239.255.0.1 | Multicast group address |
| `--multicast-port` | 5000 | Multicast port |
| `--tickers, -t` | 1,2,3 | Comma-separated ticker IDs |
| `--interface, -i` | 0.0.0.0 | Network interface |
| `--ttl` | 1 | Multicast TTL |

### Trading Client

| Flag | Default | Description |
|------|---------|-------------|
| `--host, -H` | 127.0.0.1 | Exchange host |
| `--port, -p` | 12345 | Exchange port |
| `--strategy, -s` | market-maker | Strategy: market-maker or liquidity-taker |
| `--ticker, -t` | 1 | Ticker ID to trade |
| `--client-id, -c` | 1 | Client identifier |
| `--max-order-qty` | 100 | Maximum order size |
| `--max-position` | 1000 | Position limit |
| `--max-loss` | 100000 | Maximum loss (cents) |
| `--half-spread` | 50 | Half-spread for market maker (cents) |
| `--signal-threshold` | 0.3 | Signal threshold for liquidity taker |

---

## Low-Latency Design Principles

This implementation follows key principles from **"Building Low Latency Applications with C++"**:

1. **Pre-Allocation**: All memory allocated at startup, not in the hot path
2. **Lock-Free Structures**: SPSC queues and atomic operations avoid mutex contention
3. **Cache Awareness**: Cache-line alignment prevents false sharing
4. **Zero-Copy I/O**: Packed binary protocols transmitted directly
5. **Fixed-Point Math**: Integer arithmetic for deterministic, fast calculations
6. **Non-Blocking I/O**: Polling instead of blocking system calls
7. **Deferred Work**: Logging and formatting moved off the critical path
8. **Inline Hot Paths**: Performance-critical functions marked for inlining

---

## Dependencies

| Crate | Purpose |
|-------|---------|
| `socket2` | Low-level socket configuration (TCP_NODELAY, buffers) |
| `zerocopy` | Zero-copy serialization with `#[repr(C, packed)]` |
| `crossbeam-utils` | Atomic utilities for lock-free structures |
| `clap` | Command-line argument parsing |
| `ctrlc` | Graceful shutdown signal handling |
| `criterion` | Benchmarking framework |

---

## References

- **"Building Low Latency Applications with C++"** by Sourav Ghosh (Packt Publishing)
- [The Rust Performance Book](https://nnethercote.github.io/perf-book/)
- [Lock-Free Programming Patterns](https://www.1024cores.net/)

---

## License

This project is for educational purposes, demonstrating low-latency programming patterns in Rust.
