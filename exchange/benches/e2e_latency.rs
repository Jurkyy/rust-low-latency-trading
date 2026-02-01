// End-to-end component latency benchmarks
//
// Tests:
// - Request parsing
// - Matching engine processing
// - Market data serialization

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use exchange::protocol::{
    ClientRequest, ClientResponse, MarketUpdate,
    ClientRequestType, ClientResponseType, MarketUpdateType,
    CLIENT_REQUEST_SIZE, CLIENT_RESPONSE_SIZE, MARKET_UPDATE_SIZE,
};
use exchange::matching_engine::MatchingEngine;

/// Benchmark request parsing (zero-copy deserialization)
fn bench_request_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_request_parsing");

    // Create a sample request
    let request = ClientRequest::new(
        ClientRequestType::New,
        100,    // client_id
        1,      // ticker_id
        12345,  // order_id
        1,      // side (Buy)
        10050,  // price
        100,    // qty
    );
    let bytes = request.as_bytes();

    group.bench_function("parse_client_request", |b| {
        b.iter(|| {
            let parsed = ClientRequest::from_bytes(black_box(bytes));
            black_box(parsed)
        });
    });

    group.bench_function("parse_client_request_and_extract_type", |b| {
        b.iter(|| {
            let parsed = ClientRequest::from_bytes(black_box(bytes)).unwrap();
            let msg_type = parsed.msg_type;
            let request_type = ClientRequestType::from_u8(msg_type);
            black_box(request_type)
        });
    });

    group.bench_function("parse_client_request_extract_all_fields", |b| {
        b.iter(|| {
            let parsed = ClientRequest::from_bytes(black_box(bytes)).unwrap();
            // Extract all fields (simulating what matching engine does)
            let msg_type = parsed.msg_type;
            let client_id = parsed.client_id;
            let ticker_id = parsed.ticker_id;
            let order_id = parsed.order_id;
            let side = parsed.side;
            let price = parsed.price;
            let qty = parsed.qty;
            black_box((msg_type, client_id, ticker_id, order_id, side, price, qty))
        });
    });

    // Benchmark parsing from raw buffer (simulating network receive)
    let mut raw_buffer = [0u8; 64];
    raw_buffer[..CLIENT_REQUEST_SIZE].copy_from_slice(bytes);

    group.bench_function("parse_from_network_buffer", |b| {
        b.iter(|| {
            let slice = &raw_buffer[..CLIENT_REQUEST_SIZE];
            let parsed = ClientRequest::from_bytes(black_box(slice));
            black_box(parsed)
        });
    });

    group.finish();
}

/// Benchmark matching engine processing
fn bench_matching_engine_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_matching_engine");

    // New order processing
    group.bench_function("process_new_order_single_ticker", |b| {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);
        let mut order_id = 1u64;
        b.iter(|| {
            let request = ClientRequest::new(
                ClientRequestType::New,
                100,
                1,
                order_id,
                1, // Buy
                10050,
                100,
            );
            let result = engine.process_request(black_box(&request));
            black_box(result);
            order_id += 1;
        });
    });

    // Cancel order processing (rejection path - order doesn't exist)
    group.bench_function("process_cancel_rejected", |b| {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);
        let mut cancel_id = 1u64;
        b.iter(|| {
            let request = ClientRequest::new(
                ClientRequestType::Cancel,
                100,
                1,
                cancel_id,
                1,
                10050,
                0,
            );
            let result = engine.process_request(black_box(&request));
            black_box(result);
            cancel_id += 1;
        });
    });

    // Unknown ticker processing
    group.bench_function("process_unknown_ticker", |b| {
        let mut engine = MatchingEngine::new();
        // Don't add any tickers
        let mut order_id = 1u64;
        b.iter(|| {
            let request = ClientRequest::new(
                ClientRequestType::New,
                100,
                999, // Unknown ticker
                order_id,
                1,
                10050,
                100,
            );
            let result = engine.process_request(black_box(&request));
            black_box(result);
            order_id += 1;
        });
    });

    // Processing with multiple tickers
    for num_tickers in [1, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("process_with_tickers", num_tickers),
            num_tickers,
            |b, &n| {
                let mut engine = MatchingEngine::new();
                for i in 0..n {
                    engine.add_ticker(i as u32);
                }
                let mut order_id = 1u64;
                let mut ticker = 0u32;
                b.iter(|| {
                    let request = ClientRequest::new(
                        ClientRequestType::New,
                        100,
                        ticker,
                        order_id,
                        1,
                        10050,
                        100,
                    );
                    let result = engine.process_request(black_box(&request));
                    black_box(result);
                    order_id += 1;
                    ticker = (ticker + 1) % (n as u32);
                });
            },
        );
    }

    // Full round-trip: parse -> process
    group.bench_function("parse_and_process_new_order", |b| {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);
        let request = ClientRequest::new(
            ClientRequestType::New,
            100,
            1,
            12345,
            1,
            10050,
            100,
        );
        let bytes = request.as_bytes();
        let mut order_id = 1u64;
        b.iter(|| {
            // Parse (simulating network receive)
            let mut req_copy = *ClientRequest::from_bytes(black_box(bytes)).unwrap();
            req_copy.order_id = order_id;
            // Process
            let result = engine.process_request(&req_copy);
            black_box(result);
            order_id += 1;
        });
    });

    group.finish();
}

/// Benchmark market data serialization
fn bench_market_data_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_market_data_serialization");

    // Create MarketUpdate
    group.bench_function("create_market_update", |b| {
        let mut order_id = 1u64;
        b.iter(|| {
            let update = MarketUpdate::new(
                MarketUpdateType::Add,
                black_box(1),
                black_box(order_id),
                black_box(1),
                black_box(10050),
                black_box(100),
                black_box(order_id),
            );
            black_box(update);
            order_id += 1;
        });
    });

    // Serialize to bytes
    group.bench_function("serialize_market_update", |b| {
        let update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,
            12345,
            1,
            10050,
            100,
            12345,
        );
        b.iter(|| {
            let bytes = update.as_bytes();
            black_box(bytes)
        });
    });

    // Serialize ClientResponse
    group.bench_function("serialize_client_response", |b| {
        let response = ClientResponse::new(
            ClientResponseType::Accepted,
            100,
            1,
            12345,
            67890,
            1,
            10050,
            0,
            100,
        );
        b.iter(|| {
            let bytes = response.as_bytes();
            black_box(bytes)
        });
    });

    // Copy to network buffer (simulating send)
    group.bench_function("copy_market_update_to_buffer", |b| {
        let update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,
            12345,
            1,
            10050,
            100,
            12345,
        );
        b.iter(|| {
            let mut send_buffer = [0u8; 64];
            let bytes = update.as_bytes();
            send_buffer[..MARKET_UPDATE_SIZE].copy_from_slice(bytes);
            black_box(send_buffer)
        });
    });

    group.bench_function("copy_response_to_buffer", |b| {
        let response = ClientResponse::new(
            ClientResponseType::Accepted,
            100,
            1,
            12345,
            67890,
            1,
            10050,
            0,
            100,
        );
        b.iter(|| {
            let mut send_buffer = [0u8; 64];
            let bytes = response.as_bytes();
            send_buffer[..CLIENT_RESPONSE_SIZE].copy_from_slice(bytes);
            black_box(send_buffer)
        });
    });

    // Parse MarketUpdate (simulating market data receive)
    group.bench_function("parse_market_update", |b| {
        let update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,
            12345,
            1,
            10050,
            100,
            12345,
        );
        let bytes = update.as_bytes();
        b.iter(|| {
            let parsed = MarketUpdate::from_bytes(black_box(bytes));
            black_box(parsed)
        });
    });

    group.finish();
}

/// Benchmark full end-to-end flow
fn bench_full_e2e_flow(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_full_flow");

    // Complete flow: receive request -> parse -> process -> serialize response + market data
    group.bench_function("complete_new_order_flow", |b| {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        // Simulate incoming request bytes
        let request = ClientRequest::new(
            ClientRequestType::New,
            100,
            1,
            12345,
            1,
            10050,
            100,
        );
        let request_bytes = request.as_bytes();

        let mut recv_buffer = [0u8; 64];
        let mut response_buffer = [0u8; 64];
        let mut market_buffer = [0u8; 64];

        recv_buffer[..CLIENT_REQUEST_SIZE].copy_from_slice(request_bytes);

        let mut order_id = 1u64;
        b.iter(|| {
            // 1. Parse incoming request
            let mut req = *ClientRequest::from_bytes(&recv_buffer[..CLIENT_REQUEST_SIZE]).unwrap();
            req.order_id = order_id;

            // 2. Process through matching engine
            let (response, updates) = engine.process_request(&req);

            // 3. Serialize response
            let resp_bytes = response.as_bytes();
            response_buffer[..CLIENT_RESPONSE_SIZE].copy_from_slice(resp_bytes);

            // 4. Serialize market updates
            for update in &updates {
                let upd_bytes = update.as_bytes();
                market_buffer[..MARKET_UPDATE_SIZE].copy_from_slice(upd_bytes);
            }

            black_box((&response_buffer, &market_buffer));
            order_id += 1;
        });
    });

    // Batch processing (multiple requests)
    for batch_size in [1, 10, 100].iter() {
        group.bench_with_input(
            BenchmarkId::new("batch_order_flow", batch_size),
            batch_size,
            |b, &size| {
                let mut engine = MatchingEngine::new();
                engine.add_ticker(1);

                // Pre-create requests
                let requests: Vec<ClientRequest> = (0..size)
                    .map(|i| {
                        ClientRequest::new(
                            ClientRequestType::New,
                            100,
                            1,
                            i as u64,
                            if i % 2 == 0 { 1 } else { -1 },
                            10000 + (i as i64 % 100),
                            100,
                        )
                    })
                    .collect();

                let mut next_id = size as u64;
                b.iter(|| {
                    for (i, req) in requests.iter().enumerate() {
                        let mut req_copy = *req;
                        req_copy.order_id = next_id + i as u64;
                        let result = engine.process_request(&req_copy);
                        black_box(result);
                    }
                    next_id += size as u64;
                });
            },
        );
    }

    group.finish();
}

/// Benchmark message size validation
fn bench_message_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_message_sizes");

    group.bench_function("client_request_size_check", |b| {
        b.iter(|| {
            black_box(CLIENT_REQUEST_SIZE)
        });
    });

    group.bench_function("client_response_size_check", |b| {
        b.iter(|| {
            black_box(CLIENT_RESPONSE_SIZE)
        });
    });

    group.bench_function("market_update_size_check", |b| {
        b.iter(|| {
            black_box(MARKET_UPDATE_SIZE)
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_request_parsing,
    bench_matching_engine_processing,
    bench_market_data_serialization,
    bench_full_e2e_flow,
    bench_message_sizes,
);

criterion_main!(benches);
