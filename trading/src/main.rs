//! Trading client entry point.
//!
//! This binary starts the trading client which consists of:
//! - MarketDataReceiver: Multicast market data subscription
//! - OrderGateway: TCP connection to exchange
//! - FeatureEngine: Signal generation
//! - RiskManager: Pre-trade risk checks
//! - Trading strategies (MarketMaker or LiquidityTaker)

use clap::{Parser, ValueEnum};
use common::time::now_nanos;
use common::Side;
use exchange::protocol::ClientResponseType;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use trading::features::FeatureEngine;
use trading::market_data::MarketDataReceiver;
use trading::order_gateway::OrderGateway;
use trading::position::PositionKeeper;
use trading::risk::{RiskLimits, RiskManager};
use trading::strategies::{
    LiquidityTaker, LiquidityTakerConfig, MarketMaker, MarketMakerConfig, StrategyAction,
};

/// Trading strategy to use
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Strategy {
    /// Market maker strategy - provides liquidity
    MarketMaker,
    /// Liquidity taker strategy - aggressive execution
    LiquidityTaker,
}

/// Trading client for low-latency trading
#[derive(Parser, Debug)]
#[command(name = "trading")]
#[command(about = "Low-latency trading client")]
struct Args {
    /// Exchange server host
    #[arg(short = 'H', long, default_value = "127.0.0.1")]
    host: String,

    /// Exchange server port
    #[arg(short, long, default_value_t = 12345)]
    port: u16,

    /// Multicast address for market data
    #[arg(short, long, default_value = "239.255.0.1")]
    multicast_addr: String,

    /// Multicast port for market data
    #[arg(long, default_value_t = 5000)]
    multicast_port: u16,

    /// Network interface to bind to for multicast
    #[arg(short, long, default_value = "0.0.0.0")]
    interface: String,

    /// Trading strategy to use
    #[arg(short, long, value_enum, default_value_t = Strategy::MarketMaker)]
    strategy: Strategy,

    /// Ticker ID to trade
    #[arg(short, long, default_value_t = 1)]
    ticker: u32,

    /// Client ID for this trading session
    #[arg(short, long, default_value_t = 1)]
    client_id: u32,

    /// Maximum order quantity per order
    #[arg(long, default_value_t = 100)]
    max_order_qty: u32,

    /// Maximum position (absolute value)
    #[arg(long, default_value_t = 1000)]
    max_position: i64,

    /// Maximum loss in cents (triggers risk shutdown)
    #[arg(long, default_value_t = 100000)]
    max_loss: i64,

    /// Half spread for market maker (in cents)
    #[arg(long, default_value_t = 50)]
    half_spread: i64,

    /// Signal threshold for liquidity taker
    #[arg(long, default_value_t = 0.3)]
    signal_threshold: f64,
}

fn main() {
    let args = Args::parse();

    println!("Starting trading client...");
    println!("  Exchange: {}:{}", args.host, args.port);
    println!(
        "  Multicast: {}:{}",
        args.multicast_addr, args.multicast_port
    );
    println!("  Ticker: {}", args.ticker);
    println!("  Strategy: {:?}", args.strategy);
    println!("  Client ID: {}", args.client_id);

    // Initialize market data receiver
    let mut market_data_receiver =
        match MarketDataReceiver::new(&args.multicast_addr, args.multicast_port, &args.interface) {
            Ok(receiver) => receiver,
            Err(e) => {
                eprintln!("Failed to create market data receiver: {}", e);
                std::process::exit(1);
            }
        };

    // Pre-allocate BBO for our ticker
    market_data_receiver.reserve_tickers(&[args.ticker]);

    // Initialize order gateway
    let mut order_gateway = match OrderGateway::connect(&args.host, args.port, args.client_id) {
        Ok(gateway) => gateway,
        Err(e) => {
            eprintln!("Failed to connect to exchange: {}", e);
            std::process::exit(1);
        }
    };

    // Initialize feature engine
    let mut feature_engine = FeatureEngine::new();
    feature_engine.reserve_tickers(&[args.ticker]);

    // Initialize position keeper
    let mut position_keeper = PositionKeeper::new();

    // Initialize risk manager
    let risk_limits = RiskLimits::new(
        args.max_order_qty,
        args.max_position,
        args.max_loss,
        100, // max open orders
    );
    let mut risk_manager = RiskManager::new();
    risk_manager.set_limits(args.ticker, risk_limits);

    // Initialize trading strategy
    let mut market_maker: Option<MarketMaker> = None;
    let mut liquidity_taker: Option<LiquidityTaker> = None;

    match args.strategy {
        Strategy::MarketMaker => {
            let config = MarketMakerConfig::new(args.ticker)
                .with_half_spread(args.half_spread)
                .with_base_qty(args.max_order_qty)
                .with_max_position(args.max_position);
            market_maker = Some(MarketMaker::new(config));
            println!("  Half spread: {} cents", args.half_spread);
        }
        Strategy::LiquidityTaker => {
            let config = LiquidityTakerConfig::new(args.ticker)
                .with_threshold(args.signal_threshold)
                .with_base_qty(args.max_order_qty)
                .with_max_position(args.max_position);
            liquidity_taker = Some(LiquidityTaker::new(config));
            println!("  Signal threshold: {}", args.signal_threshold);
        }
    }

    // Set up graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    ctrlc::set_handler(move || {
        println!("\nShutdown signal received...");
        running_clone.store(false, Ordering::SeqCst);
    })
    .expect("Failed to set Ctrl-C handler");

    println!("Trading client running. Press Ctrl-C to stop.");

    // Main event loop
    let mut stats_interval = 0u64;
    let mut orders_sent = 0u64;
    let mut fills_received = 0u64;

    while running.load(Ordering::SeqCst) {
        // 1. Process incoming market data
        let updates_processed = market_data_receiver.poll_and_process();

        // 2. Update feature engine with new BBO if we got updates
        if updates_processed > 0 {
            if let Some(bbo) = market_data_receiver.get_bbo(args.ticker) {
                feature_engine.on_bbo_update(args.ticker, bbo);

                // Update position keeper with market price
                if bbo.is_valid() {
                    let mid = (bbo.bid_price + bbo.ask_price) / 2;
                    position_keeper.update_market_price(args.ticker, mid);
                }
            }
        }

        // 3. Process order responses
        while let Some(response) = order_gateway.poll() {
            let response_type = response.response_type();

            match response_type {
                Some(ClientResponseType::Filled) => {
                    fills_received += 1;
                    let side = if response.side == 1 {
                        Side::Buy
                    } else {
                        Side::Sell
                    };
                    let qty = response.exec_qty;
                    let price = response.price;

                    // Update position
                    position_keeper.on_fill(args.ticker, side, qty, price);

                    // Update strategy position
                    let pos = position_keeper
                        .get_position(args.ticker)
                        .map(|p| p.position)
                        .unwrap_or(0);
                    if let Some(ref mut mm) = market_maker {
                        mm.set_position(pos);
                    }
                    if let Some(ref mut lt) = liquidity_taker {
                        lt.set_position(pos);
                        lt.on_fill();
                    }
                }
                Some(ClientResponseType::Accepted) => {
                    // Order accepted, track in position
                    let side = if response.side == 1 {
                        Side::Buy
                    } else {
                        Side::Sell
                    };
                    let pos = position_keeper.get_position_mut(args.ticker);
                    pos.add_open_order(side, response.leaves_qty);
                }
                Some(ClientResponseType::Canceled) | Some(ClientResponseType::CancelRejected) => {
                    // Remove from open orders
                    let side = if response.side == 1 {
                        Side::Buy
                    } else {
                        Side::Sell
                    };
                    let pos = position_keeper.get_position_mut(args.ticker);
                    pos.remove_open_order(side, response.leaves_qty);
                }
                _ => {}
            }
        }

        // 4. Run trading strategy
        if let Some(features) = feature_engine.get_features(args.ticker) {
            if features.is_valid() {
                // Check risk before generating orders
                let position = position_keeper.get_position_mut(args.ticker);
                let risk_ok = risk_manager.check_position(position).is_allowed();

                if risk_ok {
                    let action = match (&mut market_maker, &mut liquidity_taker) {
                        (Some(ref mut mm), None) => mm.on_features(features),
                        (None, Some(ref mut lt)) => {
                            if let Some(bbo) = market_data_receiver.get_bbo(args.ticker) {
                                lt.on_features(
                                    features,
                                    now_nanos().as_u64(),
                                    bbo.bid_price,
                                    bbo.ask_price,
                                )
                            } else {
                                StrategyAction::None
                            }
                        }
                        _ => StrategyAction::None,
                    };

                    // Execute strategy action
                    match action {
                        StrategyAction::Quote(quote_pair) => {
                            // Send bid order
                            if let Some(bid) = quote_pair.bid {
                                let risk_result = risk_manager.check_order(
                                    position,
                                    bid.side,
                                    bid.qty,
                                    bid.price,
                                );
                                if risk_result.is_allowed() {
                                    order_gateway.send_new_order(
                                        bid.ticker_id,
                                        bid.side,
                                        bid.price,
                                        bid.qty,
                                    );
                                    orders_sent += 1;
                                }
                            }
                            // Send ask order
                            if let Some(ask) = quote_pair.ask {
                                let risk_result = risk_manager.check_order(
                                    position,
                                    ask.side,
                                    ask.qty,
                                    ask.price,
                                );
                                if risk_result.is_allowed() {
                                    order_gateway.send_new_order(
                                        ask.ticker_id,
                                        ask.side,
                                        ask.price,
                                        ask.qty,
                                    );
                                    orders_sent += 1;
                                }
                            }
                        }
                        StrategyAction::Take(order) => {
                            let risk_result = risk_manager.check_order(
                                position,
                                order.side,
                                order.qty,
                                order.price,
                            );
                            if risk_result.is_allowed() {
                                order_gateway.send_new_order(
                                    order.ticker_id,
                                    order.side,
                                    order.price,
                                    order.qty,
                                );
                                orders_sent += 1;
                            }
                        }
                        StrategyAction::CancelAll(_ticker_id) => {
                            // In a full implementation, would track and cancel all open orders
                        }
                        StrategyAction::None => {}
                    }
                }
            }
        }

        // Print stats periodically
        stats_interval += 1;
        if stats_interval % 100000 == 0 {
            let pnl = position_keeper.total_pnl();
            let pos = position_keeper
                .get_position(args.ticker)
                .map(|p| p.position)
                .unwrap_or(0);
            println!(
                "Stats: pos={}, pnl={}, orders={}, fills={}, pending={}",
                pos,
                pnl,
                orders_sent,
                fills_received,
                order_gateway.pending_count()
            );
        }

        // Small sleep to prevent busy-waiting when idle
        thread::sleep(Duration::from_micros(10));
    }

    // Graceful shutdown
    println!("Shutting down...");
    let final_pnl = position_keeper.total_pnl();
    let final_pos = position_keeper
        .get_position(args.ticker)
        .map(|p| p.position)
        .unwrap_or(0);
    println!(
        "Final stats: position={}, P&L={} cents, orders_sent={}, fills={}",
        final_pos, final_pnl, orders_sent, fills_received
    );
}
