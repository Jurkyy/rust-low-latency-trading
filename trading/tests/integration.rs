//! Integration tests for the low-latency trading system.
//!
//! These tests verify end-to-end functionality across multiple components:
//! - Order flow through the matching engine
//! - Order cancellation workflow
//! - Trading client component integration (features, risk, positions)
//! - Strategy integration (market maker, liquidity taker)

use common::{Price, Qty, Side, TickerId};
use exchange::matching_engine::MatchingEngine;
use exchange::protocol::{
    ClientRequest, ClientRequestType, ClientResponse, ClientResponseType, MarketUpdate,
    MarketUpdateType,
};
use trading::features::{FeatureEngine, TickerFeatures};
use trading::market_data::BBO;
use trading::position::PositionKeeper;
use trading::risk::{RiskCheckResult, RiskLimits, RiskManager};
use trading::strategies::{
    LiquidityTaker, LiquidityTakerConfig, MarketMaker, MarketMakerConfig, OrderRequest, QuotePair,
    StrategyAction,
};
use trading::trade_engine::{TradeEngine, TradeEngineConfig};

// =============================================================================
// Test Helpers
// =============================================================================

/// Creates a valid BBO with specified prices and quantities.
fn make_bbo(bid_price: Price, bid_qty: Qty, ask_price: Price, ask_qty: Qty) -> BBO {
    BBO {
        bid_price,
        bid_qty,
        ask_price,
        ask_qty,
    }
}

/// Creates ticker features for testing.
fn make_features(
    ticker_id: TickerId,
    fair_value: Price,
    spread: Price,
    imbalance: f64,
    trade_signal: f64,
) -> TickerFeatures {
    TickerFeatures {
        ticker_id,
        fair_value,
        spread,
        mid_price: fair_value,
        imbalance,
        trade_signal,
    }
}

/// Creates a fill response for testing.
fn make_fill_response(
    client_order_id: u64,
    ticker_id: TickerId,
    side: Side,
    price: Price,
    exec_qty: Qty,
    leaves_qty: Qty,
) -> ClientResponse {
    ClientResponse::new(
        ClientResponseType::Filled,
        1, // client_id
        ticker_id,
        client_order_id,
        1000, // market_order_id
        side as i8,
        price,
        exec_qty,
        leaves_qty,
    )
}

// =============================================================================
// Order Flow End-to-End Tests
// =============================================================================

mod order_flow_tests {
    use super::*;

    #[test]
    fn test_submit_new_order_accepted() {
        // Set up matching engine with a ticker
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        // Create a new order request
        let request = ClientRequest::new(
            ClientRequestType::New,
            100,    // client_id
            1,      // ticker_id
            12345,  // order_id
            1,      // side (Buy)
            10050,  // price
            100,    // qty
        );

        // Process the request
        let (response, updates) = engine.process_request(&request);

        // Copy fields from packed struct to avoid unaligned reference issues
        let resp_msg_type = response.msg_type;
        let resp_client_id = response.client_id;
        let resp_ticker_id = response.ticker_id;
        let resp_client_order_id = response.client_order_id;
        let resp_market_order_id = response.market_order_id;
        let resp_leaves_qty = response.leaves_qty;
        let resp_exec_qty = response.exec_qty;

        // Verify order accepted response
        assert_eq!(resp_msg_type, ClientResponseType::Accepted as u8);
        assert_eq!(resp_client_id, 100);
        assert_eq!(resp_ticker_id, 1);
        assert_eq!(resp_client_order_id, 12345);
        assert_eq!(resp_market_order_id, 1); // First order ID assigned
        assert_eq!(resp_leaves_qty, 100);
        assert_eq!(resp_exec_qty, 0);

        // Verify market data update generated
        assert_eq!(updates.len(), 1);
        let update = &updates[0];
        let upd_msg_type = update.msg_type;
        let upd_ticker_id = update.ticker_id;
        let upd_order_id = update.order_id;
        let upd_side = update.side;
        let upd_price = update.price;
        let upd_qty = update.qty;

        assert_eq!(upd_msg_type, MarketUpdateType::Add as u8);
        assert_eq!(upd_ticker_id, 1);
        assert_eq!(upd_order_id, 1);
        assert_eq!(upd_side, 1);
        assert_eq!(upd_price, 10050);
        assert_eq!(upd_qty, 100);
    }

    #[test]
    fn test_submit_multiple_orders_different_sides() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        // Submit buy order
        let buy_request = ClientRequest::new(
            ClientRequestType::New,
            100,
            1,
            1001,
            1,      // Buy
            10000,
            100,
        );
        let (buy_response, buy_updates) = engine.process_request(&buy_request);
        let buy_msg_type = buy_response.msg_type;
        let buy_market_order_id = buy_response.market_order_id;
        assert_eq!(buy_msg_type, ClientResponseType::Accepted as u8);
        assert_eq!(buy_updates.len(), 1);

        // Submit sell order
        let sell_request = ClientRequest::new(
            ClientRequestType::New,
            100,
            1,
            1002,
            -1,     // Sell
            10100,
            50,
        );
        let (sell_response, sell_updates) = engine.process_request(&sell_request);
        let sell_msg_type = sell_response.msg_type;
        let sell_market_order_id = sell_response.market_order_id;
        assert_eq!(sell_msg_type, ClientResponseType::Accepted as u8);
        assert_eq!(sell_updates.len(), 1);

        // Verify order IDs are incremented
        assert_eq!(buy_market_order_id, 1);
        assert_eq!(sell_market_order_id, 2);
    }

    #[test]
    fn test_order_rejected_unknown_ticker() {
        let mut engine = MatchingEngine::new();
        // Don't add any tickers

        let request = ClientRequest::new(
            ClientRequestType::New,
            100,
            999, // Unknown ticker
            12345,
            1,
            10050,
            100,
        );

        let (response, updates) = engine.process_request(&request);

        // Verify rejection
        let msg_type = response.msg_type;
        assert_eq!(msg_type, ClientResponseType::InvalidRequest as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_order_rejected_invalid_side() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        let request = ClientRequest::new(
            ClientRequestType::New,
            100,
            1,
            12345,
            0,      // Invalid side
            10050,
            100,
        );

        let (response, updates) = engine.process_request(&request);

        let msg_type = response.msg_type;
        assert_eq!(msg_type, ClientResponseType::InvalidRequest as u8);
        assert!(updates.is_empty());
    }
}

// =============================================================================
// Order Cancellation Tests
// =============================================================================

mod order_cancellation_tests {
    use super::*;

    // These tests verify order cancellation functionality in the matching engine.
    // The cancel_order implementation correctly removes orders from the order book
    // and generates appropriate responses and market updates.

    #[test]
    fn test_cancel_request_processing() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        // First, submit an order
        let new_request = ClientRequest::new(
            ClientRequestType::New,
            100,
            1,
            12345,
            1,      // Buy
            10050,
            100,
        );
        let (new_response, _) = engine.process_request(&new_request);
        let market_order_id = new_response.market_order_id;

        // Now cancel the order
        let cancel_request = ClientRequest::new(
            ClientRequestType::Cancel,
            100,
            1,
            market_order_id,
            1,
            10050,
            0,
        );

        let (cancel_response, cancel_updates) = engine.process_request(&cancel_request);

        // Verify successful cancellation response
        let cancel_msg_type = cancel_response.msg_type;
        let cancel_client_order_id = cancel_response.client_order_id;
        assert_eq!(cancel_msg_type, ClientResponseType::Canceled as u8);
        assert_eq!(cancel_client_order_id, market_order_id);

        // Verify market update for cancellation
        assert_eq!(cancel_updates.len(), 1);
        let update = &cancel_updates[0];
        let upd_msg_type = update.msg_type;
        assert_eq!(upd_msg_type, MarketUpdateType::Cancel as u8);
    }

    #[test]
    fn test_cancel_nonexistent_order_rejected() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        // Try to cancel an order that doesn't exist
        let cancel_request = ClientRequest::new(
            ClientRequestType::Cancel,
            100,
            1,
            99999, // Non-existent order
            1,
            10050,
            0,
        );

        let (response, updates) = engine.process_request(&cancel_request);

        let msg_type = response.msg_type;
        assert_eq!(msg_type, ClientResponseType::CancelRejected as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_cancel_unknown_ticker_rejected() {
        let mut engine = MatchingEngine::new();
        // Don't add any tickers

        let cancel_request = ClientRequest::new(
            ClientRequestType::Cancel,
            100,
            999, // Unknown ticker
            12345,
            1,
            10050,
            0,
        );

        let (response, updates) = engine.process_request(&cancel_request);

        let msg_type = response.msg_type;
        assert_eq!(msg_type, ClientResponseType::CancelRejected as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_double_cancel_second_rejected() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        // Submit an order
        let new_request = ClientRequest::new(
            ClientRequestType::New,
            100,
            1,
            12345,
            1,
            10050,
            100,
        );
        let (new_response, _) = engine.process_request(&new_request);
        let market_order_id = new_response.market_order_id;

        // First cancel should succeed
        let cancel_request = ClientRequest::new(
            ClientRequestType::Cancel,
            100,
            1,
            market_order_id,
            1,
            10050,
            0,
        );
        let (first_cancel, first_updates) = engine.process_request(&cancel_request);
        let first_cancel_msg_type = first_cancel.msg_type;
        assert_eq!(first_cancel_msg_type, ClientResponseType::Canceled as u8);

        // First cancel should generate a market update
        assert_eq!(first_updates.len(), 1);
        let upd_msg_type = first_updates[0].msg_type;
        assert_eq!(upd_msg_type, MarketUpdateType::Cancel as u8);

        // Second cancel should be rejected (order already canceled)
        let (second_cancel, second_updates) = engine.process_request(&cancel_request);
        let second_cancel_msg_type = second_cancel.msg_type;
        assert_eq!(second_cancel_msg_type, ClientResponseType::CancelRejected as u8);
        assert!(second_updates.is_empty());
    }
}

// =============================================================================
// Trading Client Component Tests
// =============================================================================

mod trading_client_tests {
    use super::*;

    #[test]
    fn test_feature_engine_computes_signals_from_bbo() {
        let mut feature_engine = FeatureEngine::new();

        // Update with a valid BBO
        let bbo = make_bbo(10000, 100, 10100, 50);
        feature_engine.on_bbo_update(1, &bbo);

        // Verify features computed
        let features = feature_engine.get_features(1).expect("Features should exist");
        assert!(features.is_valid());

        // Check mid price calculation
        assert_eq!(features.mid_price, 10050); // (10000 + 10100) / 2

        // Check spread
        assert_eq!(features.spread, 100); // 10100 - 10000

        // Check imbalance (100 - 50) / (100 + 50) = 50/150 = 0.333...
        let expected_imbalance = (100.0 - 50.0) / (100.0 + 50.0);
        assert!((features.imbalance - expected_imbalance).abs() < 0.001);

        // Fair value should be initialized to mid price on first update
        assert_eq!(features.fair_value, 10050);
    }

    #[test]
    fn test_feature_engine_ema_fair_value() {
        let mut feature_engine = FeatureEngine::with_alpha(0.5);

        // First update
        let bbo1 = make_bbo(10000, 100, 10200, 100);
        feature_engine.on_bbo_update(1, &bbo1);
        assert_eq!(feature_engine.get_features(1).unwrap().fair_value, 10100);

        // Second update with higher price
        // EMA: 0.5 * 10300 + 0.5 * 10100 = 10200
        let bbo2 = make_bbo(10200, 100, 10400, 100);
        feature_engine.on_bbo_update(1, &bbo2);
        assert_eq!(feature_engine.get_features(1).unwrap().fair_value, 10200);
    }

    #[test]
    fn test_risk_manager_validates_orders() {
        let mut risk_manager = RiskManager::new();

        // Set limits for ticker 1
        risk_manager.set_limits(
            1,
            RiskLimits::new(
                100,    // max_order_qty
                1000,   // max_position
                100000, // max_loss
                10,     // max_open_orders
            ),
        );

        // Create a position
        let position = trading::position::Position::new(1);

        // Test order within limits
        let result = risk_manager.check_order(&position, Side::Buy, 50, 10000);
        assert_eq!(result, RiskCheckResult::Allowed);

        // Test order exceeding max qty
        let result = risk_manager.check_order(&position, Side::Buy, 200, 10000);
        assert_eq!(result, RiskCheckResult::OrderTooLarge);
    }

    #[test]
    fn test_risk_manager_position_limits() {
        let risk_manager = RiskManager::new();

        // Create a position near the limit (default max_position = 10000)
        let mut position = trading::position::Position::new(1);
        position.position = 9500;

        // Should reject order that would exceed position limit
        let result = risk_manager.check_order(&position, Side::Buy, 600, 10000);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);

        // Should allow order within limit
        let result = risk_manager.check_order(&position, Side::Buy, 500, 10000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_position_keeper_tracks_fills() {
        let mut position_keeper = PositionKeeper::new();

        // Execute a buy fill
        position_keeper.on_fill(1, Side::Buy, 100, 10000);

        let position = position_keeper.get_position(1).expect("Position should exist");
        assert_eq!(position.position, 100);
        assert_eq!(position.avg_open_price, 10000);
        assert_eq!(position.volume_traded, 100);

        // Execute a partial sell
        position_keeper.on_fill(1, Side::Sell, 50, 10100);

        let position = position_keeper.get_position(1).unwrap();
        assert_eq!(position.position, 50);
        assert_eq!(position.volume_traded, 150);
        // Realized P&L: (10100 - 10000) * 50 = 5000
        assert_eq!(position.realized_pnl, 5000);
    }

    #[test]
    fn test_position_keeper_unrealized_pnl() {
        let mut position_keeper = PositionKeeper::new();

        // Open a long position
        position_keeper.on_fill(1, Side::Buy, 100, 10000);

        // Update market price
        position_keeper.update_market_price(1, 10500);

        let position = position_keeper.get_position(1).unwrap();
        // Unrealized P&L: (10500 - 10000) * 100 = 50000
        assert_eq!(position.unrealized_pnl, 50000);
        assert_eq!(position.total_pnl(), 50000);
    }

    #[test]
    fn test_trade_engine_integration() {
        // Create trade engine with risk disabled for simplicity
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1])
            .with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        // Update BBO
        let bbo = make_bbo(10000, 100, 10100, 50);
        engine.update_bbo(1, bbo);

        // Verify features are computed
        let features = engine.get_features(1).expect("Features should exist");
        assert!(features.is_valid());
        assert_eq!(features.mid_price, 10050);

        // Submit an order
        let result = engine.submit_order(1, Side::Buy, 10000, 100);
        assert!(result.is_ok());

        let order_id = result.unwrap();
        assert!(engine.get_pending_order(order_id).is_some());
        assert_eq!(engine.pending_order_count(1), 1);

        // Simulate fill response
        let fill = make_fill_response(order_id, 1, Side::Buy, 10000, 100, 0);
        engine.on_response(&fill);

        // Order should be removed (fully filled)
        assert!(engine.get_pending_order(order_id).is_none());
        assert_eq!(engine.pending_order_count(1), 0);

        // Position should be updated
        let position = engine.get_position(1).unwrap();
        assert_eq!(position.position, 100);
    }
}

// =============================================================================
// Strategy Integration Tests
// =============================================================================

mod strategy_integration_tests {
    use super::*;

    #[test]
    fn test_market_maker_generates_quotes_from_features() {
        let config = MarketMakerConfig::new(1)
            .with_half_spread(50)
            .with_base_qty(100);
        let mut market_maker = MarketMaker::new(config);

        // Create features with valid market data
        let features = make_features(1, 10000, 100, 0.0, 0.0);

        // Generate quotes
        let action = market_maker.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                assert!(pair.is_two_sided());

                let bid = pair.bid.expect("Should have bid");
                let ask = pair.ask.expect("Should have ask");

                // Bid should be below fair value
                assert!(bid.price < 10000);
                // Ask should be above fair value
                assert!(ask.price > 10000);
                // Both should have base quantity
                assert_eq!(bid.qty, 100);
                assert_eq!(ask.qty, 100);
                // Verify sides
                assert_eq!(bid.side, Side::Buy);
                assert_eq!(ask.side, Side::Sell);
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_market_maker_position_skew() {
        let config = MarketMakerConfig::new(1)
            .with_base_qty(100)
            .with_position_skew(0.5)
            .with_max_position(1000);
        let mut market_maker = MarketMaker::new(config);

        // Set a long position (50% of max)
        market_maker.set_position(500);

        let features = make_features(1, 10000, 100, 0.0, 0.0);
        let action = market_maker.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                let bid = pair.bid.expect("Should have bid");
                let ask = pair.ask.expect("Should have ask");

                // With long position, bid qty should be reduced
                assert!(bid.qty < 100, "Bid qty {} should be < 100", bid.qty);
                // Ask qty should be at least base (helps reduce position)
                assert!(ask.qty >= 100, "Ask qty {} should be >= 100", ask.qty);
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_market_maker_stops_at_max_position() {
        let config = MarketMakerConfig::new(1)
            .with_base_qty(100)
            .with_max_position(1000);
        let mut market_maker = MarketMaker::new(config);

        // Set position at max
        market_maker.set_position(1000);

        let features = make_features(1, 10000, 100, 0.0, 0.0);
        let action = market_maker.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                // Should not quote bid (can't buy more)
                assert!(pair.bid.is_none(), "Should not quote bid at max position");
                // Should still quote ask
                assert!(pair.ask.is_some(), "Should still quote ask");
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_liquidity_taker_generates_orders_on_signals() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_base_qty(100)
            .with_signal_scaling(false);
        let mut liquidity_taker = LiquidityTaker::new(config);

        // Strong buy signal
        let features = make_features(1, 10000, 100, 0.0, 0.5);
        let action = liquidity_taker.on_features_simple(&features, 1_000_000_000);

        match action {
            StrategyAction::Take(order) => {
                assert_eq!(order.side, Side::Buy);
                assert_eq!(order.ticker_id, 1);
                assert_eq!(order.qty, 100);
            }
            _ => panic!("Expected Take action for buy signal"),
        }
    }

    #[test]
    fn test_liquidity_taker_sell_signal() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_signal_scaling(false);
        let mut liquidity_taker = LiquidityTaker::new(config);

        // Strong sell signal (negative)
        let features = make_features(1, 10000, 100, 0.0, -0.5);
        let action = liquidity_taker.on_features_simple(&features, 1_000_000_000);

        match action {
            StrategyAction::Take(order) => {
                assert_eq!(order.side, Side::Sell);
                assert_eq!(order.ticker_id, 1);
            }
            _ => panic!("Expected Take action for sell signal"),
        }
    }

    #[test]
    fn test_liquidity_taker_no_action_below_threshold() {
        let config = LiquidityTakerConfig::new(1).with_threshold(0.5);
        let mut liquidity_taker = LiquidityTaker::new(config);

        // Weak signal below threshold
        let features = make_features(1, 10000, 100, 0.0, 0.3);
        let action = liquidity_taker.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_liquidity_taker_rate_limiting() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_min_interval_ns(100_000_000); // 100ms
        let mut liquidity_taker = LiquidityTaker::new(config);

        let features = make_features(1, 10000, 100, 0.0, 0.5);

        // First order should go through
        let action1 = liquidity_taker.on_features_simple(&features, 1_000_000_000);
        assert!(matches!(action1, StrategyAction::Take(_)));

        // Immediate second order should be blocked
        let action2 = liquidity_taker.on_features_simple(&features, 1_000_000_001);
        assert!(matches!(action2, StrategyAction::None));

        // After interval (with cooldown), should go through
        let action3 = liquidity_taker.on_features_simple(&features, 1_500_000_000);
        assert!(matches!(action3, StrategyAction::Take(_)));
    }

    #[test]
    fn test_liquidity_taker_position_limits() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_max_position(1000);
        let mut liquidity_taker = LiquidityTaker::new(config);

        // Set position at max
        liquidity_taker.set_position(1000);

        // Strong buy signal should be blocked
        let features = make_features(1, 10000, 100, 0.0, 0.8);
        let action = liquidity_taker.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_liquidity_taker_signal_scaling() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_base_qty(100)
            .with_max_qty(500)
            .with_signal_scaling(true);
        let mut liquidity_taker = LiquidityTaker::new(config);

        // Signal at threshold
        let features_low = make_features(1, 10000, 100, 0.0, 0.31);
        let action_low = liquidity_taker.on_features_simple(&features_low, 1_000_000_000);

        let qty_low = match action_low {
            StrategyAction::Take(order) => order.qty,
            _ => panic!("Expected Take action"),
        };

        // Reset for next test
        liquidity_taker.reset();

        // Maximum signal
        let features_high = make_features(1, 10000, 100, 0.0, 1.0);
        let action_high = liquidity_taker.on_features_simple(&features_high, 2_000_000_000);

        let qty_high = match action_high {
            StrategyAction::Take(order) => order.qty,
            _ => panic!("Expected Take action"),
        };

        // Higher signal should result in larger quantity
        assert!(qty_high > qty_low, "qty_high {} should be > qty_low {}", qty_high, qty_low);
        assert_eq!(qty_high, 500, "Max signal should use max_qty");
    }

    #[test]
    fn test_trade_engine_processes_strategy_actions() {
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1])
            .with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        // Process a quote action
        let bid = OrderRequest::buy(1, 10000, 100);
        let ask = OrderRequest::sell(1, 10100, 100);
        let quote_pair = QuotePair::new(bid, ask);

        let results = engine.process_strategy_action(StrategyAction::Quote(quote_pair));

        assert_eq!(results.len(), 2);
        assert!(results[0].0.is_some()); // Bid order ID
        assert!(results[1].0.is_some()); // Ask order ID
        assert_eq!(engine.pending_order_count(1), 2);
    }

    #[test]
    fn test_trade_engine_processes_take_action() {
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1])
            .with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        // Process a take action
        let order = OrderRequest::buy(1, 10100, 200);
        let results = engine.process_strategy_action(StrategyAction::Take(order));

        assert_eq!(results.len(), 1);
        assert!(results[0].0.is_some());
        assert_eq!(results[0].1, RiskCheckResult::Allowed);
        assert_eq!(engine.pending_order_count(1), 1);
    }
}

// =============================================================================
// Full System Integration Tests
// =============================================================================

mod full_system_tests {
    use super::*;

    #[test]
    fn test_order_flow_with_market_data_update() {
        // This test simulates a complete flow:
        // 1. Submit order to matching engine
        // 2. Receive market data update
        // 3. Update trade engine with market data
        // 4. Compute features
        // 5. Generate strategy action

        // Set up matching engine
        let mut matching_engine = MatchingEngine::new();
        matching_engine.add_ticker(1);

        // Set up trade engine
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1])
            .with_risk_checks(false);
        let mut trade_engine = TradeEngine::new(config);

        // Submit buy order to matching engine
        let buy_request = ClientRequest::new(
            ClientRequestType::New,
            1,
            1,
            1001,
            1,      // Buy
            10000,
            100,
        );
        let (_, buy_updates) = matching_engine.process_request(&buy_request);

        // Submit sell order
        let sell_request = ClientRequest::new(
            ClientRequestType::New,
            1,
            1,
            1002,
            -1,     // Sell
            10100,
            50,
        );
        let (_, sell_updates) = matching_engine.process_request(&sell_request);

        // Process market data updates in trade engine
        for update in buy_updates.iter().chain(sell_updates.iter()) {
            trade_engine.on_market_update(update);
        }

        // Verify BBO is updated
        let bbo = trade_engine.get_bbo(1).expect("BBO should exist");
        assert_eq!(bbo.bid_price, 10000);
        assert_eq!(bbo.bid_qty, 100);
        assert_eq!(bbo.ask_price, 10100);
        assert_eq!(bbo.ask_qty, 50);

        // Verify features are computed
        let features = trade_engine.get_features(1).expect("Features should exist");
        assert!(features.is_valid());
        assert_eq!(features.mid_price, 10050);
        assert_eq!(features.spread, 100);
    }

    #[test]
    fn test_full_trading_cycle() {
        // Set up trade engine
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1])
            .with_risk_checks(false);
        let mut trade_engine = TradeEngine::new(config);
        trade_engine.start();

        // Set up market maker
        let mm_config = MarketMakerConfig::new(1)
            .with_half_spread(50)
            .with_base_qty(100);
        let mut market_maker = MarketMaker::new(mm_config);

        // Update market data
        let bbo = make_bbo(10000, 100, 10100, 50);
        trade_engine.update_bbo(1, bbo);

        // Get features and generate quotes
        let features = trade_engine.get_features(1).unwrap().clone();
        let action = market_maker.on_features(&features);

        // Process strategy action
        match action {
            StrategyAction::Quote(pair) => {
                let results = trade_engine.process_strategy_action(StrategyAction::Quote(pair));
                assert_eq!(results.len(), 2);

                // Both orders should be submitted
                assert!(results.iter().all(|(id, _)| id.is_some()));
            }
            _ => panic!("Expected Quote action"),
        }

        // Verify orders are tracked
        assert_eq!(trade_engine.pending_order_count(1), 2);

        // Simulate fill for first order
        // The order IDs start from 1 in our test setup
        let fill = make_fill_response(1, 1, Side::Buy, 9950, 100, 0);
        trade_engine.on_response(&fill);

        // Verify position updated
        let position = trade_engine.get_position(1).unwrap();
        assert_eq!(position.position, 100);

        // Verify stats
        assert_eq!(trade_engine.stats().fills_received, 1);
    }

    #[test]
    fn test_run_cycle_processes_events() {
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1])
            .with_risk_checks(false);
        let mut trade_engine = TradeEngine::new(config);
        trade_engine.start();

        // Submit an order first
        let order_id = trade_engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        // Create some events
        let responses = vec![
            make_fill_response(order_id, 1, Side::Buy, 10000, 100, 0),
        ];
        let updates: Vec<MarketUpdate> = vec![
            MarketUpdate::new(MarketUpdateType::Add, 1, 1, 1, 10000, 100, 1),
            MarketUpdate::new(MarketUpdateType::Add, 1, 2, -1, 10100, 50, 2),
        ];

        // Run cycle
        let processed = trade_engine.run_cycle(responses.into_iter(), updates.into_iter());

        assert_eq!(processed, 3);
        assert_eq!(trade_engine.stats().responses_processed, 1);
        assert_eq!(trade_engine.stats().market_updates_processed, 2);
        assert_eq!(trade_engine.stats().total_cycles, 1);
    }

    #[test]
    fn test_risk_blocked_order_in_strategy_flow() {
        // Set up trade engine with risk enabled
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1])
            .with_risk_checks(true);
        let mut trade_engine = TradeEngine::new(config);

        // Set tight limits
        trade_engine.risk_manager_mut().set_limits(
            1,
            RiskLimits::new(50, 100, 10000, 5), // max_order_qty = 50
        );

        // Try to submit an order larger than limit
        let order = OrderRequest::buy(1, 10000, 100);
        let results = trade_engine.process_strategy_action(StrategyAction::Take(order));

        // Order should be rejected
        assert_eq!(results.len(), 1);
        assert!(results[0].0.is_none()); // No order ID
        assert_eq!(results[0].1, RiskCheckResult::OrderTooLarge);
        assert_eq!(trade_engine.stats().orders_rejected_risk, 1);
    }
}
