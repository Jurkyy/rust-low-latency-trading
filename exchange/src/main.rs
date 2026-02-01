//! Exchange server entry point.
//!
//! This binary starts the exchange server which consists of:
//! - OrderServer: TCP gateway for client connections
//! - MatchingEngine: Order routing and execution
//! - MarketDataPublisher: Multicast market data feed

use clap::Parser;
use exchange::market_data::{MarketDataPublisher, MarketDataPublisherConfig};
use exchange::matching_engine::MatchingEngine;
use exchange::order_server::{OrderServer, OrderServerConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Exchange server for low-latency trading
#[derive(Parser, Debug)]
#[command(name = "exchange")]
#[command(about = "Low-latency trading exchange server")]
struct Args {
    /// TCP port for client connections
    #[arg(short, long, default_value_t = 12345)]
    port: u16,

    /// Multicast address for market data
    #[arg(short, long, default_value = "239.255.0.1")]
    multicast_addr: String,

    /// Multicast port for market data
    #[arg(long, default_value_t = 5000)]
    multicast_port: u16,

    /// Comma-separated list of ticker IDs to support
    #[arg(short, long, default_value = "1,2,3")]
    tickers: String,

    /// Network interface to bind to
    #[arg(short, long, default_value = "0.0.0.0")]
    interface: String,

    /// Multicast TTL (time-to-live)
    #[arg(long, default_value_t = 1)]
    ttl: u32,
}

fn parse_tickers(tickers_str: &str) -> Vec<u32> {
    tickers_str
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .collect()
}

fn main() {
    let args = Args::parse();

    println!("Starting exchange server...");
    println!("  TCP port: {}", args.port);
    println!("  Multicast: {}:{}", args.multicast_addr, args.multicast_port);
    println!("  Interface: {}", args.interface);

    // Parse ticker IDs
    let tickers = parse_tickers(&args.tickers);
    if tickers.is_empty() {
        eprintln!("Error: No valid ticker IDs provided");
        std::process::exit(1);
    }
    println!("  Tickers: {:?}", tickers);

    // Initialize components
    let order_server_config = OrderServerConfig::new(&args.interface, args.port);
    let mut order_server = match OrderServer::new(order_server_config) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("Failed to create order server: {}", e);
            std::process::exit(1);
        }
    };

    let mut matching_engine = MatchingEngine::new();
    for &ticker_id in &tickers {
        matching_engine.add_ticker(ticker_id);
    }

    let md_config = MarketDataPublisherConfig {
        multicast_addr: args.multicast_addr.clone(),
        port: args.multicast_port,
        interface: args.interface.clone(),
        ttl: args.ttl,
        enable_snapshots: true,
        snapshot_interval: 1000,
    };

    let mut market_data_publisher = match MarketDataPublisher::new(md_config) {
        Ok(publisher) => publisher,
        Err(e) => {
            eprintln!("Failed to create market data publisher: {}", e);
            std::process::exit(1);
        }
    };

    // Register tickers with market data publisher
    for &ticker_id in &tickers {
        market_data_publisher.register_ticker(ticker_id);
    }

    // Set up graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    ctrlc::set_handler(move || {
        println!("\nShutdown signal received...");
        running_clone.store(false, Ordering::SeqCst);
    })
    .expect("Failed to set Ctrl-C handler");

    println!("Exchange server running. Press Ctrl-C to stop.");

    // Main event loop
    let mut stats_interval = 0u64;
    while running.load(Ordering::SeqCst) {
        // Poll for incoming client requests
        let requests = order_server.poll();

        for seq_request in requests {
            // Process request through matching engine
            let (response, market_updates) =
                matching_engine.process_request(&seq_request.request);

            // Send response back to client
            if let Err(e) = order_server.send_response(seq_request.client_id, &response) {
                eprintln!(
                    "Failed to send response to client {}: {}",
                    seq_request.client_id, e
                );
            }

            // Publish market data updates
            for update in &market_updates {
                if let Err(e) = market_data_publisher.publish(update) {
                    eprintln!("Failed to publish market update: {}", e);
                }
            }
        }

        // Print stats periodically
        stats_interval += 1;
        if stats_interval % 100000 == 0 {
            println!(
                "Stats: clients={}, seq={}, md_updates={}",
                order_server.client_count(),
                order_server.current_sequence(),
                market_data_publisher.total_updates_sent()
            );
        }

        // Small sleep to prevent busy-waiting when idle
        // In production, this would use epoll/kqueue for efficient waiting
        thread::sleep(Duration::from_micros(10));
    }

    // Graceful shutdown
    println!("Shutting down...");
    order_server.disconnect_all();
    println!(
        "Exchange server stopped. Total updates sent: {}",
        market_data_publisher.total_updates_sent()
    );
}
