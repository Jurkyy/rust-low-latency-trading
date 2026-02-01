//! Central trading orchestrator for the low-latency trading system.
//!
//! The TradeEngine is the central component that wires together all trading
//! subsystems and implements the main event processing loop. It handles:
//!
//! - Market data reception and processing
//! - Feature computation from market data
//! - Strategy signal generation
//! - Risk management checks
//! - Order submission and lifecycle management
//! - Position tracking from fill responses
//!
//! The event loop prioritizes processing in this order:
//! 1. Exchange responses (highest priority - need to track order state)
//! 2. Market data updates (need fresh prices for decisions)
//! 3. Strategy signals (based on updated market state)

use std::collections::HashMap;

use common::time::{now_nanos, Nanos};
use common::{ClientId, OrderId, Price, Qty, Side, TickerId};
use exchange::protocol::{ClientResponse, ClientResponseType, MarketUpdate};

use crate::features::{FeatureEngine, TickerFeatures};
use crate::market_data::BBO;
use crate::position::{Position, PositionKeeper};
use crate::risk::{RiskCheckResult, RiskManager};
use crate::strategies::{OrderRequest, StrategyAction};

/// Configuration for the TradeEngine.
#[derive(Debug, Clone)]
pub struct TradeEngineConfig {
    /// Client identifier for this trading session.
    pub client_id: ClientId,
    /// List of tickers to trade.
    pub tickers: Vec<TickerId>,
    /// Whether to enable risk checks (can be disabled for testing).
    pub enable_risk_checks: bool,
    /// Maximum number of events to process per poll cycle.
    pub max_events_per_cycle: usize,
}

impl Default for TradeEngineConfig {
    fn default() -> Self {
        Self {
            client_id: 1,
            tickers: Vec::new(),
            enable_risk_checks: true,
            max_events_per_cycle: 100,
        }
    }
}

impl TradeEngineConfig {
    /// Creates a new config with the given client ID.
    pub fn new(client_id: ClientId) -> Self {
        Self {
            client_id,
            ..Default::default()
        }
    }

    /// Builder method to set the tickers to trade.
    pub fn with_tickers(mut self, tickers: Vec<TickerId>) -> Self {
        self.tickers = tickers;
        self
    }

    /// Builder method to enable/disable risk checks.
    pub fn with_risk_checks(mut self, enabled: bool) -> Self {
        self.enable_risk_checks = enabled;
        self
    }

    /// Builder method to set max events per cycle.
    pub fn with_max_events_per_cycle(mut self, max: usize) -> Self {
        self.max_events_per_cycle = max;
        self
    }
}

/// Statistics for tracking engine performance.
#[derive(Debug, Clone, Default)]
pub struct TradeEngineStats {
    /// Number of market data updates processed.
    pub market_updates_processed: u64,
    /// Number of exchange responses processed.
    pub responses_processed: u64,
    /// Number of orders submitted.
    pub orders_submitted: u64,
    /// Number of orders rejected by risk.
    pub orders_rejected_risk: u64,
    /// Number of fills received.
    pub fills_received: u64,
    /// Number of strategy cycles run.
    pub strategy_cycles: u64,
    /// Total processing cycles.
    pub total_cycles: u64,
}

impl TradeEngineStats {
    /// Creates new empty stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resets all statistics.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Represents a pending order tracked by the engine.
#[derive(Debug, Clone)]
pub struct TrackedOrder {
    /// The order ID.
    pub order_id: OrderId,
    /// The ticker being traded.
    pub ticker_id: TickerId,
    /// The side of the order.
    pub side: Side,
    /// The order price.
    pub price: Price,
    /// The original quantity.
    pub original_qty: Qty,
    /// The remaining quantity.
    pub leaves_qty: Qty,
    /// When the order was sent.
    pub sent_time: Nanos,
}

/// Callback type for order submission.
/// Takes (ticker_id, side, price, qty) and returns the assigned order_id.
pub type OrderSubmitCallback = Box<dyn FnMut(TickerId, Side, Price, Qty) -> OrderId + Send>;

/// Callback type for order cancellation.
/// Takes (order_id, ticker_id).
pub type OrderCancelCallback = Box<dyn FnMut(OrderId, TickerId) + Send>;

/// Central trading orchestrator.
///
/// The TradeEngine coordinates all trading components:
/// - Processes market data and updates features
/// - Runs strategy logic to generate signals
/// - Validates orders against risk limits
/// - Tracks order lifecycle and positions
pub struct TradeEngine {
    /// Engine configuration.
    config: TradeEngineConfig,
    /// Feature engine for computing trading signals.
    feature_engine: FeatureEngine,
    /// Risk manager for pre-trade checks.
    risk_manager: RiskManager,
    /// Position keeper for tracking positions and P&L.
    position_keeper: PositionKeeper,
    /// BBO state per ticker.
    bbo_state: HashMap<TickerId, BBO>,
    /// Pending orders by order ID.
    pending_orders: HashMap<OrderId, TrackedOrder>,
    /// Open order count per ticker.
    open_order_count: HashMap<TickerId, u32>,
    /// Callback for submitting orders.
    order_submit_callback: Option<OrderSubmitCallback>,
    /// Callback for cancelling orders.
    order_cancel_callback: Option<OrderCancelCallback>,
    /// Engine statistics.
    stats: TradeEngineStats,
    /// Whether the engine is running.
    running: bool,
}

impl TradeEngine {
    /// Creates a new TradeEngine with the given configuration.
    pub fn new(config: TradeEngineConfig) -> Self {
        let mut engine = Self {
            config: config.clone(),
            feature_engine: FeatureEngine::new(),
            risk_manager: RiskManager::new(),
            position_keeper: PositionKeeper::new(),
            bbo_state: HashMap::new(),
            pending_orders: HashMap::new(),
            open_order_count: HashMap::new(),
            order_submit_callback: None,
            order_cancel_callback: None,
            stats: TradeEngineStats::new(),
            running: false,
        };

        // Pre-allocate state for configured tickers
        engine.feature_engine.reserve_tickers(&config.tickers);
        for &ticker_id in &config.tickers {
            engine.bbo_state.insert(ticker_id, BBO::new());
            engine.open_order_count.insert(ticker_id, 0);
        }

        engine
    }

    /// Creates a TradeEngine with default configuration.
    pub fn with_defaults(client_id: ClientId) -> Self {
        Self::new(TradeEngineConfig::new(client_id))
    }

    // ========================================================================
    // Configuration and Setup
    // ========================================================================

    /// Sets the order submission callback.
    pub fn set_order_submit_callback(&mut self, callback: OrderSubmitCallback) {
        self.order_submit_callback = Some(callback);
    }

    /// Sets the order cancellation callback.
    pub fn set_order_cancel_callback(&mut self, callback: OrderCancelCallback) {
        self.order_cancel_callback = Some(callback);
    }

    /// Returns a reference to the risk manager.
    pub fn risk_manager(&self) -> &RiskManager {
        &self.risk_manager
    }

    /// Returns a mutable reference to the risk manager.
    pub fn risk_manager_mut(&mut self) -> &mut RiskManager {
        &mut self.risk_manager
    }

    /// Returns a reference to the feature engine.
    pub fn feature_engine(&self) -> &FeatureEngine {
        &self.feature_engine
    }

    /// Returns a mutable reference to the feature engine.
    pub fn feature_engine_mut(&mut self) -> &mut FeatureEngine {
        &mut self.feature_engine
    }

    /// Returns a reference to the position keeper.
    pub fn position_keeper(&self) -> &PositionKeeper {
        &self.position_keeper
    }

    /// Returns a mutable reference to the position keeper.
    pub fn position_keeper_mut(&mut self) -> &mut PositionKeeper {
        &mut self.position_keeper
    }

    /// Returns the current statistics.
    pub fn stats(&self) -> &TradeEngineStats {
        &self.stats
    }

    /// Returns the engine configuration.
    pub fn config(&self) -> &TradeEngineConfig {
        &self.config
    }

    /// Returns whether the engine is running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Starts the engine.
    pub fn start(&mut self) {
        self.running = true;
    }

    /// Stops the engine.
    pub fn stop(&mut self) {
        self.running = false;
    }

    // ========================================================================
    // Market Data Processing
    // ========================================================================

    /// Processes a market data update.
    ///
    /// Updates the internal BBO state and feature engine.
    /// Returns the ticker ID if the update was processed successfully.
    pub fn on_market_update(&mut self, update: &MarketUpdate) -> Option<TickerId> {
        let ticker_id = update.ticker_id;
        let side = update.side;
        let price = update.price;
        let qty = update.qty;

        // Get or create BBO entry
        let bbo = self.bbo_state.entry(ticker_id).or_insert_with(BBO::new);

        // Update BBO based on update type
        if let Some(update_type) = update.update_type() {
            use exchange::protocol::MarketUpdateType;
            match update_type {
                MarketUpdateType::Add | MarketUpdateType::Modify | MarketUpdateType::Snapshot => {
                    if side == Side::Buy as i8 {
                        if price > bbo.bid_price || bbo.bid_price == common::INVALID_PRICE {
                            bbo.bid_price = price;
                            bbo.bid_qty = qty;
                        } else if price == bbo.bid_price {
                            bbo.bid_qty = qty;
                        }
                    } else if side == Side::Sell as i8 {
                        if price < bbo.ask_price || bbo.ask_price == common::INVALID_PRICE {
                            bbo.ask_price = price;
                            bbo.ask_qty = qty;
                        } else if price == bbo.ask_price {
                            bbo.ask_qty = qty;
                        }
                    }
                }
                MarketUpdateType::Cancel => {
                    if side == Side::Buy as i8 && price == bbo.bid_price {
                        if qty == 0 || qty >= bbo.bid_qty {
                            bbo.bid_qty = 0;
                        } else {
                            bbo.bid_qty = bbo.bid_qty.saturating_sub(qty);
                        }
                    } else if side == Side::Sell as i8 && price == bbo.ask_price {
                        if qty == 0 || qty >= bbo.ask_qty {
                            bbo.ask_qty = 0;
                        } else {
                            bbo.ask_qty = bbo.ask_qty.saturating_sub(qty);
                        }
                    }
                }
                MarketUpdateType::Trade => {
                    if side == Side::Buy as i8 && price == bbo.ask_price {
                        bbo.ask_qty = bbo.ask_qty.saturating_sub(qty);
                    } else if side == Side::Sell as i8 && price == bbo.bid_price {
                        bbo.bid_qty = bbo.bid_qty.saturating_sub(qty);
                    }

                    // Update position keeper with market price
                    self.position_keeper.update_market_price(ticker_id, price);
                }
                MarketUpdateType::Clear => {
                    *bbo = BBO::new();
                }
            }
        }

        // Update feature engine with new BBO
        self.feature_engine.on_bbo_update(ticker_id, bbo);

        self.stats.market_updates_processed += 1;

        Some(ticker_id)
    }

    /// Updates the BBO directly (for testing or alternative data sources).
    pub fn update_bbo(&mut self, ticker_id: TickerId, bbo: BBO) {
        self.bbo_state.insert(ticker_id, bbo);
        self.feature_engine.on_bbo_update(ticker_id, &bbo);

        // Update position keeper with mid price if valid
        if let Some(mid) = bbo.mid_price() {
            self.position_keeper.update_market_price(ticker_id, mid);
        }
    }

    /// Returns the current BBO for a ticker.
    pub fn get_bbo(&self, ticker_id: TickerId) -> Option<&BBO> {
        self.bbo_state.get(&ticker_id)
    }

    // ========================================================================
    // Response Processing
    // ========================================================================

    /// Processes an exchange response.
    ///
    /// Updates order state, positions, and handles fills/cancels.
    pub fn on_response(&mut self, response: &ClientResponse) {
        let client_order_id = response.client_order_id;
        let ticker_id = response.ticker_id;
        let exec_qty = response.exec_qty;
        let price = response.price;
        let leaves_qty = response.leaves_qty;

        self.stats.responses_processed += 1;

        if let Some(response_type) = response.response_type() {
            match response_type {
                ClientResponseType::Accepted => {
                    // Order accepted - already tracked from submission
                }
                ClientResponseType::Filled => {
                    // Process the fill
                    if let Some(order) = self.pending_orders.get(&client_order_id) {
                        let side = order.side;

                        // Update position
                        self.position_keeper.on_fill(ticker_id, side, exec_qty, price);

                        // Remove pending order quantity from position tracker
                        let position = self.position_keeper.get_position_mut(ticker_id);
                        position.remove_open_order(side, exec_qty);

                        self.stats.fills_received += 1;
                    }

                    // Update or remove the tracked order
                    if leaves_qty == 0 {
                        // Fully filled - remove order
                        self.pending_orders.remove(&client_order_id);
                        let count = self.open_order_count.entry(ticker_id).or_insert(0);
                        *count = count.saturating_sub(1);
                    } else if let Some(order) = self.pending_orders.get_mut(&client_order_id) {
                        // Partially filled - update leaves qty
                        order.leaves_qty = leaves_qty;
                    }
                }
                ClientResponseType::Canceled => {
                    // Order canceled - remove from tracking
                    if let Some(order) = self.pending_orders.remove(&client_order_id) {
                        // Remove pending order quantity from position tracker
                        let position = self.position_keeper.get_position_mut(ticker_id);
                        position.remove_open_order(order.side, order.leaves_qty);

                        let count = self.open_order_count.entry(ticker_id).or_insert(0);
                        *count = count.saturating_sub(1);
                    }
                }
                ClientResponseType::CancelRejected | ClientResponseType::InvalidRequest => {
                    // Remove from tracking on rejection
                    if let Some(order) = self.pending_orders.remove(&client_order_id) {
                        let position = self.position_keeper.get_position_mut(ticker_id);
                        position.remove_open_order(order.side, order.leaves_qty);

                        let count = self.open_order_count.entry(ticker_id).or_insert(0);
                        *count = count.saturating_sub(1);
                    }
                }
            }
        }
    }

    // ========================================================================
    // Order Management
    // ========================================================================

    /// Checks if an order passes risk validation.
    ///
    /// Returns the risk check result.
    pub fn check_order_risk(
        &self,
        ticker_id: TickerId,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> RiskCheckResult {
        if !self.config.enable_risk_checks {
            return RiskCheckResult::Allowed;
        }

        let position = self
            .position_keeper
            .get_position(ticker_id)
            .cloned()
            .unwrap_or_else(|| Position::new(ticker_id));

        let open_orders = *self.open_order_count.get(&ticker_id).unwrap_or(&0);

        self.risk_manager
            .check_order_with_open_orders(&position, side, qty, price, open_orders)
    }

    /// Submits an order after risk validation.
    ///
    /// Returns the order ID if successful, or the risk rejection reason.
    pub fn submit_order(
        &mut self,
        ticker_id: TickerId,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> Result<OrderId, RiskCheckResult> {
        // Check risk
        let risk_result = self.check_order_risk(ticker_id, side, price, qty);
        if !risk_result.is_allowed() {
            self.stats.orders_rejected_risk += 1;
            return Err(risk_result);
        }

        // Submit via callback
        let order_id = if let Some(callback) = &mut self.order_submit_callback {
            callback(ticker_id, side, price, qty)
        } else {
            // No callback - generate a placeholder ID
            self.stats.orders_submitted + 1
        };

        // Track the order
        let tracked = TrackedOrder {
            order_id,
            ticker_id,
            side,
            price,
            original_qty: qty,
            leaves_qty: qty,
            sent_time: now_nanos(),
        };
        self.pending_orders.insert(order_id, tracked);

        // Update open order count
        *self.open_order_count.entry(ticker_id).or_insert(0) += 1;

        // Add pending order to position tracker
        let position = self.position_keeper.get_position_mut(ticker_id);
        position.add_open_order(side, qty);

        self.stats.orders_submitted += 1;

        Ok(order_id)
    }

    /// Cancels an order.
    pub fn cancel_order(&mut self, order_id: OrderId) {
        if let Some(order) = self.pending_orders.get(&order_id) {
            let ticker_id = order.ticker_id;
            if let Some(callback) = &mut self.order_cancel_callback {
                callback(order_id, ticker_id);
            }
        }
    }

    /// Cancels all orders for a ticker.
    pub fn cancel_all_orders(&mut self, ticker_id: TickerId) {
        let order_ids: Vec<OrderId> = self
            .pending_orders
            .iter()
            .filter(|(_, o)| o.ticker_id == ticker_id)
            .map(|(&id, _)| id)
            .collect();

        for order_id in order_ids {
            self.cancel_order(order_id);
        }
    }

    /// Returns a reference to a pending order.
    pub fn get_pending_order(&self, order_id: OrderId) -> Option<&TrackedOrder> {
        self.pending_orders.get(&order_id)
    }

    /// Returns the number of pending orders for a ticker.
    pub fn pending_order_count(&self, ticker_id: TickerId) -> u32 {
        *self.open_order_count.get(&ticker_id).unwrap_or(&0)
    }

    /// Returns the total number of pending orders.
    pub fn total_pending_orders(&self) -> usize {
        self.pending_orders.len()
    }

    // ========================================================================
    // Strategy Integration
    // ========================================================================

    /// Processes a strategy action.
    ///
    /// Validates orders against risk and submits them.
    /// Returns a vector of (OrderId, RiskCheckResult) for each order attempted.
    pub fn process_strategy_action(
        &mut self,
        action: StrategyAction,
    ) -> Vec<(Option<OrderId>, RiskCheckResult)> {
        let mut results = Vec::new();

        match action {
            StrategyAction::None => {}
            StrategyAction::Quote(pair) => {
                // Process bid
                if let Some(bid) = pair.bid {
                    let result = self.submit_order(bid.ticker_id, bid.side, bid.price, bid.qty);
                    match result {
                        Ok(id) => results.push((Some(id), RiskCheckResult::Allowed)),
                        Err(risk) => results.push((None, risk)),
                    }
                }
                // Process ask
                if let Some(ask) = pair.ask {
                    let result = self.submit_order(ask.ticker_id, ask.side, ask.price, ask.qty);
                    match result {
                        Ok(id) => results.push((Some(id), RiskCheckResult::Allowed)),
                        Err(risk) => results.push((None, risk)),
                    }
                }
            }
            StrategyAction::Take(order) => {
                let result =
                    self.submit_order(order.ticker_id, order.side, order.price, order.qty);
                match result {
                    Ok(id) => results.push((Some(id), RiskCheckResult::Allowed)),
                    Err(risk) => results.push((None, risk)),
                }
            }
            StrategyAction::CancelAll(ticker_id) => {
                self.cancel_all_orders(ticker_id);
            }
        }

        self.stats.strategy_cycles += 1;
        results
    }

    /// Processes an order request.
    ///
    /// Convenience method for submitting a single order request.
    pub fn process_order_request(
        &mut self,
        request: &OrderRequest,
    ) -> Result<OrderId, RiskCheckResult> {
        self.submit_order(request.ticker_id, request.side, request.price, request.qty)
    }

    /// Gets the current features for a ticker.
    pub fn get_features(&self, ticker_id: TickerId) -> Option<&TickerFeatures> {
        self.feature_engine.get_features(ticker_id)
    }

    /// Gets the current position for a ticker.
    pub fn get_position(&self, ticker_id: TickerId) -> Option<&Position> {
        self.position_keeper.get_position(ticker_id)
    }

    // ========================================================================
    // Event Loop Support
    // ========================================================================

    /// Runs a single processing cycle.
    ///
    /// This method should be called repeatedly in the main event loop.
    /// It processes pending events in priority order:
    /// 1. Exchange responses
    /// 2. Market data
    /// 3. Strategy signals
    ///
    /// The responses and market_data iterators should be provided by the caller
    /// who is polling the network connections.
    pub fn run_cycle<R, M>(
        &mut self,
        responses: R,
        market_updates: M,
    ) -> usize
    where
        R: Iterator<Item = ClientResponse>,
        M: Iterator<Item = MarketUpdate>,
    {
        if !self.running {
            return 0;
        }

        let mut events_processed = 0;
        let max_events = self.config.max_events_per_cycle;

        // Priority 1: Process exchange responses
        for response in responses.take(max_events) {
            self.on_response(&response);
            events_processed += 1;
        }

        // Priority 2: Process market data
        let remaining = max_events.saturating_sub(events_processed);
        for update in market_updates.take(remaining) {
            self.on_market_update(&update);
            events_processed += 1;
        }

        self.stats.total_cycles += 1;

        events_processed
    }

    /// Resets the engine state (for testing or recovery).
    pub fn reset(&mut self) {
        self.feature_engine.clear();
        self.bbo_state.clear();
        self.pending_orders.clear();
        self.open_order_count.clear();
        self.stats.reset();

        // Re-initialize for configured tickers
        self.feature_engine.reserve_tickers(&self.config.tickers);
        for &ticker_id in &self.config.tickers {
            self.bbo_state.insert(ticker_id, BBO::new());
            self.open_order_count.insert(ticker_id, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::protocol::MarketUpdateType;

    fn make_bbo(bid_price: Price, bid_qty: Qty, ask_price: Price, ask_qty: Qty) -> BBO {
        BBO {
            bid_price,
            bid_qty,
            ask_price,
            ask_qty,
        }
    }

    fn make_market_update(
        ticker_id: TickerId,
        update_type: MarketUpdateType,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> MarketUpdate {
        MarketUpdate::new(update_type, ticker_id, 1, side as i8, price, qty, 1)
    }

    fn make_fill_response(
        client_order_id: OrderId,
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

    fn make_accepted_response(
        client_order_id: OrderId,
        ticker_id: TickerId,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> ClientResponse {
        ClientResponse::new(
            ClientResponseType::Accepted,
            1,
            ticker_id,
            client_order_id,
            1000,
            side as i8,
            price,
            0,
            qty,
        )
    }

    fn make_canceled_response(
        client_order_id: OrderId,
        ticker_id: TickerId,
    ) -> ClientResponse {
        ClientResponse::new(
            ClientResponseType::Canceled,
            1,
            ticker_id,
            client_order_id,
            1000,
            1,
            0,
            0,
            0,
        )
    }

    // ========================================================================
    // Configuration Tests
    // ========================================================================

    #[test]
    fn test_config_default() {
        let config = TradeEngineConfig::default();
        assert_eq!(config.client_id, 1);
        assert!(config.tickers.is_empty());
        assert!(config.enable_risk_checks);
        assert_eq!(config.max_events_per_cycle, 100);
    }

    #[test]
    fn test_config_builder() {
        let config = TradeEngineConfig::new(42)
            .with_tickers(vec![1, 2, 3])
            .with_risk_checks(false)
            .with_max_events_per_cycle(50);

        assert_eq!(config.client_id, 42);
        assert_eq!(config.tickers, vec![1, 2, 3]);
        assert!(!config.enable_risk_checks);
        assert_eq!(config.max_events_per_cycle, 50);
    }

    // ========================================================================
    // Engine Construction Tests
    // ========================================================================

    #[test]
    fn test_engine_new() {
        let config = TradeEngineConfig::new(1).with_tickers(vec![1, 2]);
        let engine = TradeEngine::new(config);

        assert!(!engine.is_running());
        assert_eq!(engine.stats().market_updates_processed, 0);
        assert_eq!(engine.total_pending_orders(), 0);
    }

    #[test]
    fn test_engine_start_stop() {
        let mut engine = TradeEngine::with_defaults(1);

        assert!(!engine.is_running());
        engine.start();
        assert!(engine.is_running());
        engine.stop();
        assert!(!engine.is_running());
    }

    // ========================================================================
    // Market Data Processing Tests
    // ========================================================================

    #[test]
    fn test_on_market_update_add() {
        let mut engine = TradeEngine::with_defaults(1);

        let update = make_market_update(1, MarketUpdateType::Add, Side::Buy, 10000, 100);
        engine.on_market_update(&update);

        let bbo = engine.get_bbo(1).unwrap();
        assert_eq!(bbo.bid_price, 10000);
        assert_eq!(bbo.bid_qty, 100);

        assert_eq!(engine.stats().market_updates_processed, 1);
    }

    #[test]
    fn test_on_market_update_both_sides() {
        let mut engine = TradeEngine::with_defaults(1);

        let bid_update = make_market_update(1, MarketUpdateType::Add, Side::Buy, 10000, 100);
        let ask_update = make_market_update(1, MarketUpdateType::Add, Side::Sell, 10100, 50);

        engine.on_market_update(&bid_update);
        engine.on_market_update(&ask_update);

        let bbo = engine.get_bbo(1).unwrap();
        assert_eq!(bbo.bid_price, 10000);
        assert_eq!(bbo.bid_qty, 100);
        assert_eq!(bbo.ask_price, 10100);
        assert_eq!(bbo.ask_qty, 50);
        assert!(bbo.is_valid());
    }

    #[test]
    fn test_on_market_update_trade() {
        let mut engine = TradeEngine::with_defaults(1);

        // Set up initial BBO
        engine.update_bbo(1, make_bbo(10000, 100, 10100, 50));

        // Trade at the ask
        let trade = make_market_update(1, MarketUpdateType::Trade, Side::Buy, 10100, 20);
        engine.on_market_update(&trade);

        let bbo = engine.get_bbo(1).unwrap();
        assert_eq!(bbo.ask_qty, 30); // 50 - 20
    }

    #[test]
    fn test_on_market_update_clear() {
        let mut engine = TradeEngine::with_defaults(1);

        // Set up initial BBO
        engine.update_bbo(1, make_bbo(10000, 100, 10100, 50));

        // Clear the book
        let clear = make_market_update(1, MarketUpdateType::Clear, Side::Buy, 0, 0);
        engine.on_market_update(&clear);

        let bbo = engine.get_bbo(1).unwrap();
        assert!(!bbo.is_valid());
    }

    #[test]
    fn test_update_bbo_directly() {
        let mut engine = TradeEngine::with_defaults(1);

        let bbo = make_bbo(10000, 100, 10100, 50);
        engine.update_bbo(1, bbo);

        let stored = engine.get_bbo(1).unwrap();
        assert_eq!(stored.bid_price, 10000);
        assert_eq!(stored.ask_price, 10100);

        // Features should be updated
        let features = engine.get_features(1).unwrap();
        assert!(features.is_valid());
    }

    // ========================================================================
    // Order Submission Tests
    // ========================================================================

    #[test]
    fn test_submit_order_success() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let result = engine.submit_order(1, Side::Buy, 10000, 100);
        assert!(result.is_ok());

        let order_id = result.unwrap();
        assert!(engine.get_pending_order(order_id).is_some());
        assert_eq!(engine.pending_order_count(1), 1);
        assert_eq!(engine.stats().orders_submitted, 1);
    }

    #[test]
    fn test_submit_order_with_callback() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let mut next_id = 1000u64;
        engine.set_order_submit_callback(Box::new(move |_ticker, _side, _price, _qty| {
            let id = next_id;
            next_id += 1;
            id
        }));

        let result = engine.submit_order(1, Side::Buy, 10000, 100);
        assert_eq!(result.unwrap(), 1000);

        let result2 = engine.submit_order(1, Side::Sell, 10100, 50);
        assert_eq!(result2.unwrap(), 1001);
    }

    #[test]
    fn test_submit_order_risk_rejection() {
        let mut engine = TradeEngine::with_defaults(1);

        // Set tight risk limits
        engine.risk_manager_mut().set_limits(
            1,
            crate::risk::RiskLimits::new(50, 1000, 100000, 10), // max_order_qty = 50
        );

        // Try to submit order larger than limit
        let result = engine.submit_order(1, Side::Buy, 10000, 100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), RiskCheckResult::OrderTooLarge);
        assert_eq!(engine.stats().orders_rejected_risk, 1);
    }

    #[test]
    fn test_cancel_order() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let mut cancelled_ids = Vec::new();
        engine.set_order_cancel_callback(Box::new(move |id, _ticker| {
            cancelled_ids.push(id);
        }));

        let order_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();
        engine.cancel_order(order_id);

        // Order is still tracked until cancel response
        assert!(engine.get_pending_order(order_id).is_some());
    }

    #[test]
    fn test_cancel_all_orders() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        // Submit multiple orders
        engine.submit_order(1, Side::Buy, 10000, 100).unwrap();
        engine.submit_order(1, Side::Sell, 10100, 50).unwrap();
        engine.submit_order(2, Side::Buy, 20000, 200).unwrap();

        assert_eq!(engine.pending_order_count(1), 2);
        assert_eq!(engine.pending_order_count(2), 1);

        engine.cancel_all_orders(1);

        // Orders still tracked until responses
        assert_eq!(engine.pending_order_count(1), 2);
    }

    // ========================================================================
    // Response Processing Tests
    // ========================================================================

    #[test]
    fn test_on_response_accepted() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let order_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        let response = make_accepted_response(order_id, 1, Side::Buy, 10000, 100);
        engine.on_response(&response);

        assert_eq!(engine.stats().responses_processed, 1);
        assert!(engine.get_pending_order(order_id).is_some());
    }

    #[test]
    fn test_on_response_filled_full() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let order_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        // Fully filled
        let response = make_fill_response(order_id, 1, Side::Buy, 10000, 100, 0);
        engine.on_response(&response);

        assert_eq!(engine.stats().fills_received, 1);
        assert!(engine.get_pending_order(order_id).is_none()); // Removed
        assert_eq!(engine.pending_order_count(1), 0);

        // Position updated
        let position = engine.get_position(1).unwrap();
        assert_eq!(position.position, 100);
    }

    #[test]
    fn test_on_response_filled_partial() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let order_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        // Partial fill
        let response = make_fill_response(order_id, 1, Side::Buy, 10000, 60, 40);
        engine.on_response(&response);

        let order = engine.get_pending_order(order_id).unwrap();
        assert_eq!(order.leaves_qty, 40);
        assert_eq!(engine.pending_order_count(1), 1); // Still open

        let position = engine.get_position(1).unwrap();
        assert_eq!(position.position, 60);
    }

    #[test]
    fn test_on_response_canceled() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let order_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        let response = make_canceled_response(order_id, 1);
        engine.on_response(&response);

        assert!(engine.get_pending_order(order_id).is_none());
        assert_eq!(engine.pending_order_count(1), 0);
    }

    // ========================================================================
    // Strategy Integration Tests
    // ========================================================================

    #[test]
    fn test_process_strategy_action_none() {
        let mut engine = TradeEngine::with_defaults(1);

        let results = engine.process_strategy_action(StrategyAction::None);
        assert!(results.is_empty());
    }

    #[test]
    fn test_process_strategy_action_take() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let order = crate::strategies::OrderRequest::buy(1, 10000, 100);
        let results = engine.process_strategy_action(StrategyAction::Take(order));

        assert_eq!(results.len(), 1);
        assert!(results[0].0.is_some());
        assert_eq!(results[0].1, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_process_strategy_action_quote() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let bid = crate::strategies::OrderRequest::buy(1, 10000, 100);
        let ask = crate::strategies::OrderRequest::sell(1, 10100, 100);
        let pair = crate::strategies::QuotePair::new(bid, ask);

        let results = engine.process_strategy_action(StrategyAction::Quote(pair));

        assert_eq!(results.len(), 2);
        assert!(results[0].0.is_some());
        assert!(results[1].0.is_some());
        assert_eq!(engine.pending_order_count(1), 2);
    }

    #[test]
    fn test_process_strategy_action_cancel_all() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        engine.submit_order(1, Side::Buy, 10000, 100).unwrap();
        engine.submit_order(1, Side::Sell, 10100, 50).unwrap();

        let results = engine.process_strategy_action(StrategyAction::CancelAll(1));
        assert!(results.is_empty()); // Cancel doesn't return results
    }

    #[test]
    fn test_process_order_request() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let request = crate::strategies::OrderRequest::buy(1, 10000, 100);
        let result = engine.process_order_request(&request);

        assert!(result.is_ok());
        assert_eq!(engine.pending_order_count(1), 1);
    }

    // ========================================================================
    // Event Loop Tests
    // ========================================================================

    #[test]
    fn test_run_cycle_not_running() {
        let mut engine = TradeEngine::with_defaults(1);

        let responses: Vec<ClientResponse> = vec![];
        let updates: Vec<MarketUpdate> = vec![];

        let processed = engine.run_cycle(responses.into_iter(), updates.into_iter());
        assert_eq!(processed, 0);
    }

    #[test]
    fn test_run_cycle_with_events() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);
        engine.start();

        // Submit an order first
        let order_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        // Create events
        let responses = vec![make_fill_response(order_id, 1, Side::Buy, 10000, 100, 0)];
        let updates = vec![
            make_market_update(1, MarketUpdateType::Add, Side::Buy, 10000, 100),
            make_market_update(1, MarketUpdateType::Add, Side::Sell, 10100, 50),
        ];

        let processed = engine.run_cycle(responses.into_iter(), updates.into_iter());
        assert_eq!(processed, 3);
        assert_eq!(engine.stats().responses_processed, 1);
        assert_eq!(engine.stats().market_updates_processed, 2);
        assert_eq!(engine.stats().total_cycles, 1);
    }

    #[test]
    fn test_run_cycle_respects_max_events() {
        let config = TradeEngineConfig::new(1)
            .with_risk_checks(false)
            .with_max_events_per_cycle(2);
        let mut engine = TradeEngine::new(config);
        engine.start();

        let updates: Vec<MarketUpdate> = (0..10)
            .map(|i| make_market_update(1, MarketUpdateType::Add, Side::Buy, 10000 + i, 100))
            .collect();

        let processed = engine.run_cycle(std::iter::empty(), updates.into_iter());
        assert_eq!(processed, 2); // Limited by max_events_per_cycle
    }

    // ========================================================================
    // Reset Tests
    // ========================================================================

    #[test]
    fn test_reset() {
        let config = TradeEngineConfig::new(1)
            .with_tickers(vec![1, 2])
            .with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        // Add some state
        engine.update_bbo(1, make_bbo(10000, 100, 10100, 50));
        engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        assert_eq!(engine.stats().orders_submitted, 1);
        assert_eq!(engine.total_pending_orders(), 1);

        engine.reset();

        assert_eq!(engine.stats().orders_submitted, 0);
        assert_eq!(engine.total_pending_orders(), 0);
        assert!(engine.get_features(1).is_none() || !engine.get_features(1).unwrap().is_valid());
    }

    // ========================================================================
    // Position Tracking Tests
    // ========================================================================

    #[test]
    fn test_position_updated_on_fill() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        let order_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();

        // Fill
        let response = make_fill_response(order_id, 1, Side::Buy, 10000, 100, 0);
        engine.on_response(&response);

        let position = engine.get_position(1).unwrap();
        assert_eq!(position.position, 100);
        assert_eq!(position.volume_traded, 100);
    }

    #[test]
    fn test_position_tracking_round_trip() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let mut engine = TradeEngine::new(config);

        // Buy
        let buy_id = engine.submit_order(1, Side::Buy, 10000, 100).unwrap();
        engine.on_response(&make_fill_response(buy_id, 1, Side::Buy, 10000, 100, 0));

        assert_eq!(engine.get_position(1).unwrap().position, 100);

        // Sell
        let sell_id = engine.submit_order(1, Side::Sell, 10100, 100).unwrap();
        engine.on_response(&make_fill_response(sell_id, 1, Side::Sell, 10100, 100, 0));

        let position = engine.get_position(1).unwrap();
        assert_eq!(position.position, 0);
        assert_eq!(position.volume_traded, 200);
        // Realized P&L: (10100 - 10000) * 100 = 10000
        assert_eq!(position.realized_pnl, 10000);
    }

    // ========================================================================
    // Risk Check Tests
    // ========================================================================

    #[test]
    fn test_check_order_risk_disabled() {
        let config = TradeEngineConfig::new(1).with_risk_checks(false);
        let engine = TradeEngine::new(config);

        // Even with huge order, should pass when risk disabled
        let result = engine.check_order_risk(1, Side::Buy, 10000, 1_000_000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_risk_enabled() {
        let engine = TradeEngine::with_defaults(1);

        // Default limits: max_order_qty = 1000
        let result = engine.check_order_risk(1, Side::Buy, 10000, 500);
        assert_eq!(result, RiskCheckResult::Allowed);

        let result = engine.check_order_risk(1, Side::Buy, 10000, 1500);
        assert_eq!(result, RiskCheckResult::OrderTooLarge);
    }

    // ========================================================================
    // Feature Engine Integration Tests
    // ========================================================================

    #[test]
    fn test_features_updated_on_bbo_change() {
        let mut engine = TradeEngine::with_defaults(1);

        // No features initially
        assert!(engine.get_features(1).is_none());

        // Update BBO
        engine.update_bbo(1, make_bbo(10000, 100, 10100, 50));

        // Features should now exist
        let features = engine.get_features(1).unwrap();
        assert!(features.is_valid());
        assert_eq!(features.mid_price, 10050);
        assert_eq!(features.spread, 100);
    }

    // ========================================================================
    // Stats Tests
    // ========================================================================

    #[test]
    fn test_stats_new() {
        let stats = TradeEngineStats::new();
        assert_eq!(stats.market_updates_processed, 0);
        assert_eq!(stats.responses_processed, 0);
        assert_eq!(stats.orders_submitted, 0);
        assert_eq!(stats.orders_rejected_risk, 0);
        assert_eq!(stats.fills_received, 0);
        assert_eq!(stats.strategy_cycles, 0);
        assert_eq!(stats.total_cycles, 0);
    }

    #[test]
    fn test_stats_reset() {
        let mut stats = TradeEngineStats::new();
        stats.market_updates_processed = 100;
        stats.orders_submitted = 50;

        stats.reset();

        assert_eq!(stats.market_updates_processed, 0);
        assert_eq!(stats.orders_submitted, 0);
    }
}
