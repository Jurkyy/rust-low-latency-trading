//! Liquidity taker strategy for aggressive execution.
//!
//! The liquidity taker monitors trading signals and aggressively takes liquidity
//! when the signal exceeds a configurable threshold. It's designed for momentum
//! or signal-based trading where speed of execution matters more than price impact.

use common::{Price, Qty, TickerId};
use crate::features::TickerFeatures;
use super::{OrderRequest, StrategyAction};

/// Configuration parameters for the liquidity taker strategy.
#[derive(Debug, Clone, Copy)]
pub struct LiquidityTakerConfig {
    /// The ticker this strategy trades.
    pub ticker_id: TickerId,
    /// Signal threshold to trigger a buy (positive signal).
    /// Takes liquidity when trade_signal > buy_threshold.
    pub buy_threshold: f64,
    /// Signal threshold to trigger a sell (negative signal).
    /// Takes liquidity when trade_signal < sell_threshold (should be negative).
    pub sell_threshold: f64,
    /// Base quantity to take when signal is at threshold.
    pub base_qty: Qty,
    /// Maximum quantity to take in a single order.
    pub max_qty: Qty,
    /// Whether to scale quantity based on signal strength.
    /// If true, larger signals result in larger order sizes.
    pub scale_with_signal: bool,
    /// Price aggression in basis points (how much to cross the spread).
    /// 0 = take at best bid/ask, positive = cross spread by this amount.
    pub aggression_bps: u32,
    /// Minimum time between orders in nanoseconds (rate limiting).
    pub min_order_interval_ns: u64,
    /// Maximum position before stopping (0 = no limit).
    pub max_position: i64,
    /// Cooldown multiplier after a trade (increases wait time).
    pub cooldown_factor: f64,
}

impl Default for LiquidityTakerConfig {
    fn default() -> Self {
        Self {
            ticker_id: 0,
            buy_threshold: 0.3,     // Buy when signal > 0.3
            sell_threshold: -0.3,   // Sell when signal < -0.3
            base_qty: 100,          // 100 shares base
            max_qty: 500,           // 500 shares max
            scale_with_signal: true,
            aggression_bps: 10,     // 10 bps aggression
            min_order_interval_ns: 100_000_000, // 100ms min interval
            max_position: 5000,     // Max 5000 shares position
            cooldown_factor: 2.0,   // Double wait time after trade
        }
    }
}

impl LiquidityTakerConfig {
    /// Creates a new liquidity taker config for a specific ticker.
    pub fn new(ticker_id: TickerId) -> Self {
        Self {
            ticker_id,
            ..Default::default()
        }
    }

    /// Builder method to set buy threshold.
    pub fn with_buy_threshold(mut self, threshold: f64) -> Self {
        self.buy_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Builder method to set sell threshold.
    pub fn with_sell_threshold(mut self, threshold: f64) -> Self {
        self.sell_threshold = threshold.clamp(-1.0, 0.0);
        self
    }

    /// Builder method to set both thresholds symmetrically.
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        let abs_threshold = threshold.abs().clamp(0.0, 1.0);
        self.buy_threshold = abs_threshold;
        self.sell_threshold = -abs_threshold;
        self
    }

    /// Builder method to set base quantity.
    pub fn with_base_qty(mut self, base_qty: Qty) -> Self {
        self.base_qty = base_qty;
        self
    }

    /// Builder method to set max quantity.
    pub fn with_max_qty(mut self, max_qty: Qty) -> Self {
        self.max_qty = max_qty;
        self
    }

    /// Builder method to enable/disable signal scaling.
    pub fn with_signal_scaling(mut self, enabled: bool) -> Self {
        self.scale_with_signal = enabled;
        self
    }

    /// Builder method to set aggression in basis points.
    pub fn with_aggression_bps(mut self, bps: u32) -> Self {
        self.aggression_bps = bps;
        self
    }

    /// Builder method to set minimum order interval.
    pub fn with_min_interval_ns(mut self, interval_ns: u64) -> Self {
        self.min_order_interval_ns = interval_ns;
        self
    }

    /// Builder method to set max position.
    pub fn with_max_position(mut self, max_position: i64) -> Self {
        self.max_position = max_position;
        self
    }

    /// Builder method to set cooldown factor.
    pub fn with_cooldown_factor(mut self, factor: f64) -> Self {
        self.cooldown_factor = factor.max(1.0);
        self
    }
}

/// Liquidity taker strategy state for a single ticker.
///
/// Monitors trading signals and generates aggressive orders when signals
/// exceed thresholds.
pub struct LiquidityTaker {
    /// Strategy configuration.
    config: LiquidityTakerConfig,
    /// Timestamp of last order in nanoseconds.
    last_order_time_ns: u64,
    /// Current effective interval (may be increased by cooldown).
    effective_interval_ns: u64,
    /// Current position (tracked externally, updated via set_position).
    current_position: i64,
    /// Whether the strategy is active.
    active: bool,
    /// Count of orders sent (for metrics).
    orders_sent: u64,
}

impl LiquidityTaker {
    /// Creates a new liquidity taker with the given configuration.
    pub fn new(config: LiquidityTakerConfig) -> Self {
        Self {
            effective_interval_ns: config.min_order_interval_ns,
            config,
            last_order_time_ns: 0,
            current_position: 0,
            active: true,
            orders_sent: 0,
        }
    }

    /// Creates a liquidity taker with default config for a ticker.
    pub fn for_ticker(ticker_id: TickerId) -> Self {
        Self::new(LiquidityTakerConfig::new(ticker_id))
    }

    /// Returns a reference to the configuration.
    #[inline]
    pub fn config(&self) -> &LiquidityTakerConfig {
        &self.config
    }

    /// Returns a mutable reference to the configuration.
    #[inline]
    pub fn config_mut(&mut self) -> &mut LiquidityTakerConfig {
        &mut self.config
    }

    /// Updates the current position (should be called when fills occur).
    #[inline]
    pub fn set_position(&mut self, position: i64) {
        self.current_position = position;
    }

    /// Returns the current position.
    #[inline]
    pub fn position(&self) -> i64 {
        self.current_position
    }

    /// Activates the strategy.
    #[inline]
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// Deactivates the strategy.
    #[inline]
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Returns whether the strategy is active.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the number of orders sent.
    #[inline]
    pub fn orders_sent(&self) -> u64 {
        self.orders_sent
    }

    /// Processes features and generates take orders if signal threshold is crossed.
    ///
    /// # Arguments
    /// * `features` - The current ticker features from the feature engine
    /// * `current_time_ns` - Current time in nanoseconds (for rate limiting)
    /// * `best_bid` - Best bid price in the market (for sell orders)
    /// * `best_ask` - Best ask price in the market (for buy orders)
    ///
    /// # Returns
    /// A `StrategyAction` indicating what action to take (if any)
    pub fn on_features(
        &mut self,
        features: &TickerFeatures,
        current_time_ns: u64,
        best_bid: Price,
        best_ask: Price,
    ) -> StrategyAction {
        // Check if strategy is active
        if !self.active {
            return StrategyAction::None;
        }

        // Check if features are valid
        if !features.is_valid() {
            return StrategyAction::None;
        }

        // Check rate limiting
        if !self.can_send_order(current_time_ns) {
            return StrategyAction::None;
        }

        // Determine if we should take liquidity based on signal
        let signal = features.trade_signal;

        // Check for buy signal
        if signal > self.config.buy_threshold {
            // Check position limit
            if self.config.max_position > 0 && self.current_position >= self.config.max_position {
                return StrategyAction::None;
            }

            // Calculate order
            if let Some(order) = self.create_buy_order(signal, best_ask) {
                self.record_order(current_time_ns);
                return StrategyAction::Take(order);
            }
        }

        // Check for sell signal
        if signal < self.config.sell_threshold {
            // Check position limit
            if self.config.max_position > 0 && self.current_position <= -self.config.max_position {
                return StrategyAction::None;
            }

            // Calculate order
            if let Some(order) = self.create_sell_order(signal, best_bid) {
                self.record_order(current_time_ns);
                return StrategyAction::Take(order);
            }
        }

        StrategyAction::None
    }

    /// Simplified version for testing - uses features mid_price as reference.
    pub fn on_features_simple(&mut self, features: &TickerFeatures, current_time_ns: u64) -> StrategyAction {
        let mid = features.mid_price;
        let half_spread = features.spread / 2;
        let best_bid = mid - half_spread;
        let best_ask = mid + half_spread;
        self.on_features(features, current_time_ns, best_bid, best_ask)
    }

    /// Checks if enough time has passed to send another order.
    #[inline]
    fn can_send_order(&self, current_time_ns: u64) -> bool {
        if self.last_order_time_ns == 0 {
            return true;
        }
        current_time_ns >= self.last_order_time_ns + self.effective_interval_ns
    }

    /// Records that an order was sent and applies cooldown.
    fn record_order(&mut self, current_time_ns: u64) {
        self.last_order_time_ns = current_time_ns;
        self.orders_sent += 1;

        // Apply cooldown - increase effective interval
        self.effective_interval_ns = ((self.effective_interval_ns as f64 * self.config.cooldown_factor) as u64)
            .min(self.config.min_order_interval_ns * 10); // Cap at 10x base interval
    }

    /// Resets the cooldown timer (e.g., after a period of inactivity).
    pub fn reset_cooldown(&mut self) {
        self.effective_interval_ns = self.config.min_order_interval_ns;
    }

    /// Creates a buy order with appropriate price and quantity.
    fn create_buy_order(&self, signal: f64, best_ask: Price) -> Option<OrderRequest> {
        let qty = self.calculate_quantity(signal);
        if qty == 0 {
            return None;
        }

        // Calculate aggressive price (cross the spread)
        let aggression = (best_ask as f64 * self.config.aggression_bps as f64 / 10000.0) as Price;
        let price = best_ask + aggression;

        Some(OrderRequest::buy(self.config.ticker_id, price, qty))
    }

    /// Creates a sell order with appropriate price and quantity.
    fn create_sell_order(&self, signal: f64, best_bid: Price) -> Option<OrderRequest> {
        let qty = self.calculate_quantity(signal);
        if qty == 0 {
            return None;
        }

        // Calculate aggressive price (cross the spread)
        let aggression = (best_bid as f64 * self.config.aggression_bps as f64 / 10000.0) as Price;
        let price = best_bid - aggression;

        Some(OrderRequest::sell(self.config.ticker_id, price, qty))
    }

    /// Calculates order quantity based on signal strength.
    fn calculate_quantity(&self, signal: f64) -> Qty {
        if self.config.scale_with_signal {
            // Scale quantity based on signal strength
            // At threshold: base_qty, at max signal (1.0): max_qty
            let signal_abs = signal.abs();
            let threshold = if signal > 0.0 {
                self.config.buy_threshold
            } else {
                self.config.sell_threshold.abs()
            };

            // How far above threshold are we? (0.0 to 1.0 range)
            let signal_excess = (signal_abs - threshold) / (1.0 - threshold);
            let signal_factor = signal_excess.clamp(0.0, 1.0);

            // Linear interpolation between base and max
            let base = self.config.base_qty as f64;
            let max = self.config.max_qty as f64;
            let qty = base + (max - base) * signal_factor;

            (qty as Qty).clamp(1, self.config.max_qty)
        } else {
            self.config.base_qty
        }
    }

    /// Called when an order is filled to reset cooldown partially.
    pub fn on_fill(&mut self) {
        // After a fill, reduce cooldown by half (we got what we wanted)
        self.effective_interval_ns = self.effective_interval_ns / 2;
        self.effective_interval_ns = self.effective_interval_ns.max(self.config.min_order_interval_ns);
    }

    /// Resets the strategy state.
    pub fn reset(&mut self) {
        self.last_order_time_ns = 0;
        self.effective_interval_ns = self.config.min_order_interval_ns;
        self.orders_sent = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::Side;

    fn make_features(ticker_id: TickerId, fair_value: Price, spread: Price, trade_signal: f64) -> TickerFeatures {
        TickerFeatures {
            ticker_id,
            fair_value,
            spread,
            mid_price: fair_value,
            imbalance: 0.0,
            trade_signal,
        }
    }

    // ==================== Config Tests ====================

    #[test]
    fn test_config_default() {
        let config = LiquidityTakerConfig::default();
        assert!((config.buy_threshold - 0.3).abs() < f64::EPSILON);
        assert!((config.sell_threshold - (-0.3)).abs() < f64::EPSILON);
        assert_eq!(config.base_qty, 100);
        assert_eq!(config.max_qty, 500);
        assert!(config.scale_with_signal);
    }

    #[test]
    fn test_config_builder() {
        let config = LiquidityTakerConfig::new(1)
            .with_buy_threshold(0.5)
            .with_sell_threshold(-0.5)
            .with_base_qty(200)
            .with_max_qty(1000)
            .with_signal_scaling(false)
            .with_aggression_bps(20)
            .with_max_position(10000);

        assert_eq!(config.ticker_id, 1);
        assert!((config.buy_threshold - 0.5).abs() < f64::EPSILON);
        assert!((config.sell_threshold - (-0.5)).abs() < f64::EPSILON);
        assert_eq!(config.base_qty, 200);
        assert_eq!(config.max_qty, 1000);
        assert!(!config.scale_with_signal);
        assert_eq!(config.aggression_bps, 20);
        assert_eq!(config.max_position, 10000);
    }

    #[test]
    fn test_config_symmetric_threshold() {
        let config = LiquidityTakerConfig::new(1).with_threshold(0.4);
        assert!((config.buy_threshold - 0.4).abs() < f64::EPSILON);
        assert!((config.sell_threshold - (-0.4)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_threshold_clamping() {
        let config = LiquidityTakerConfig::new(1)
            .with_buy_threshold(1.5)
            .with_sell_threshold(-1.5);
        assert!((config.buy_threshold - 1.0).abs() < f64::EPSILON);
        assert!((config.sell_threshold - (-1.0)).abs() < f64::EPSILON);
    }

    // ==================== Liquidity Taker Construction Tests ====================

    #[test]
    fn test_liquidity_taker_new() {
        let lt = LiquidityTaker::for_ticker(1);
        assert_eq!(lt.config.ticker_id, 1);
        assert_eq!(lt.current_position, 0);
        assert!(lt.is_active());
        assert_eq!(lt.orders_sent(), 0);
    }

    #[test]
    fn test_liquidity_taker_activate_deactivate() {
        let mut lt = LiquidityTaker::for_ticker(1);
        assert!(lt.is_active());

        lt.deactivate();
        assert!(!lt.is_active());

        lt.activate();
        assert!(lt.is_active());
    }

    // ==================== Signal Threshold Tests ====================

    #[test]
    fn test_buy_signal_above_threshold() {
        let config = LiquidityTakerConfig::new(1)
            .with_buy_threshold(0.3)
            .with_signal_scaling(false);
        let mut lt = LiquidityTaker::new(config);

        // Signal above threshold
        let features = make_features(1, 10000, 100, 0.5);
        let action = lt.on_features_simple(&features, 1_000_000_000);

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
    fn test_sell_signal_below_threshold() {
        let config = LiquidityTakerConfig::new(1)
            .with_sell_threshold(-0.3)
            .with_signal_scaling(false);
        let mut lt = LiquidityTaker::new(config);

        // Signal below (negative) threshold
        let features = make_features(1, 10000, 100, -0.5);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        match action {
            StrategyAction::Take(order) => {
                assert_eq!(order.side, Side::Sell);
                assert_eq!(order.ticker_id, 1);
                assert_eq!(order.qty, 100);
            }
            _ => panic!("Expected Take action for sell signal"),
        }
    }

    #[test]
    fn test_signal_below_threshold_no_action() {
        let config = LiquidityTakerConfig::new(1).with_threshold(0.5);
        let mut lt = LiquidityTaker::new(config);

        // Signal below threshold
        let features = make_features(1, 10000, 100, 0.3);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_inactive_strategy_no_action() {
        let mut lt = LiquidityTaker::for_ticker(1);
        lt.deactivate();

        // Strong signal but inactive
        let features = make_features(1, 10000, 100, 0.8);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_invalid_features_no_action() {
        let mut lt = LiquidityTaker::for_ticker(1);

        // Invalid features
        let features = TickerFeatures::new(1);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::None));
    }

    // ==================== Signal Scaling Tests ====================

    #[test]
    fn test_signal_scaling_at_threshold() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_base_qty(100)
            .with_max_qty(500)
            .with_signal_scaling(true);
        let mut lt = LiquidityTaker::new(config);

        // Signal exactly at threshold
        let features = make_features(1, 10000, 100, 0.31);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        match action {
            StrategyAction::Take(order) => {
                // Should be close to base_qty
                assert!(order.qty >= 100 && order.qty <= 150,
                    "Qty {} should be close to base_qty at threshold", order.qty);
            }
            _ => panic!("Expected Take action"),
        }
    }

    #[test]
    fn test_signal_scaling_at_max() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_base_qty(100)
            .with_max_qty(500)
            .with_signal_scaling(true);
        let mut lt = LiquidityTaker::new(config);

        // Maximum signal
        let features = make_features(1, 10000, 100, 1.0);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        match action {
            StrategyAction::Take(order) => {
                // Should be at or near max_qty
                assert_eq!(order.qty, 500, "Should use max_qty at max signal");
            }
            _ => panic!("Expected Take action"),
        }
    }

    #[test]
    fn test_no_signal_scaling() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_base_qty(100)
            .with_max_qty(500)
            .with_signal_scaling(false);
        let mut lt = LiquidityTaker::new(config);

        // Strong signal but no scaling
        let features = make_features(1, 10000, 100, 0.9);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        match action {
            StrategyAction::Take(order) => {
                assert_eq!(order.qty, 100, "Should use base_qty when scaling disabled");
            }
            _ => panic!("Expected Take action"),
        }
    }

    // ==================== Rate Limiting Tests ====================

    #[test]
    fn test_rate_limiting() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_min_interval_ns(100_000_000); // 100ms
        let mut lt = LiquidityTaker::new(config);

        let features = make_features(1, 10000, 100, 0.5);

        // First order should go through
        let action1 = lt.on_features_simple(&features, 1_000_000_000);
        assert!(matches!(action1, StrategyAction::Take(_)));

        // Immediate second order should be blocked
        let action2 = lt.on_features_simple(&features, 1_000_000_001);
        assert!(matches!(action2, StrategyAction::None));

        // After interval should go through (but with cooldown, need longer wait)
        let action3 = lt.on_features_simple(&features, 1_500_000_000);
        assert!(matches!(action3, StrategyAction::Take(_)));
    }

    #[test]
    fn test_cooldown_increases_interval() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_min_interval_ns(100_000_000)
            .with_cooldown_factor(2.0);
        let mut lt = LiquidityTaker::new(config);

        let features = make_features(1, 10000, 100, 0.5);

        // First order
        lt.on_features_simple(&features, 1_000_000_000);
        let interval_after_first = lt.effective_interval_ns;

        // Effective interval should have increased
        assert!(interval_after_first > 100_000_000,
            "Interval {} should be > base 100_000_000 after cooldown", interval_after_first);
    }

    #[test]
    fn test_reset_cooldown() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_min_interval_ns(100_000_000)
            .with_cooldown_factor(2.0);
        let mut lt = LiquidityTaker::new(config);

        let features = make_features(1, 10000, 100, 0.5);

        // Send order to increase cooldown
        lt.on_features_simple(&features, 1_000_000_000);
        assert!(lt.effective_interval_ns > 100_000_000);

        // Reset cooldown
        lt.reset_cooldown();
        assert_eq!(lt.effective_interval_ns, 100_000_000);
    }

    // ==================== Position Limit Tests ====================

    #[test]
    fn test_max_long_position_blocks_buy() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_max_position(1000);
        let mut lt = LiquidityTaker::new(config);

        // Set position at max
        lt.set_position(1000);

        // Strong buy signal should be blocked
        let features = make_features(1, 10000, 100, 0.8);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_max_short_position_blocks_sell() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_max_position(1000);
        let mut lt = LiquidityTaker::new(config);

        // Set position at max short
        lt.set_position(-1000);

        // Strong sell signal should be blocked
        let features = make_features(1, 10000, 100, -0.8);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_long_position_allows_sell() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_max_position(1000);
        let mut lt = LiquidityTaker::new(config);

        // Long position
        lt.set_position(1000);

        // Sell signal should still work (reduces position)
        let features = make_features(1, 10000, 100, -0.8);
        let action = lt.on_features_simple(&features, 1_000_000_000);

        assert!(matches!(action, StrategyAction::Take(_)));
    }

    // ==================== Price Aggression Tests ====================

    #[test]
    fn test_buy_order_price_aggression() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_aggression_bps(10); // 10 bps = 0.1%
        let mut lt = LiquidityTaker::new(config);

        let features = make_features(1, 10000, 100, 0.5);
        // best_ask = 10000 + 50 = 10050
        let action = lt.on_features(&features, 1_000_000_000, 9950, 10050);

        match action {
            StrategyAction::Take(order) => {
                // Price should be above best_ask due to aggression
                assert!(order.price > 10050,
                    "Price {} should be above best_ask 10050", order.price);
            }
            _ => panic!("Expected Take action"),
        }
    }

    #[test]
    fn test_sell_order_price_aggression() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_aggression_bps(10);
        let mut lt = LiquidityTaker::new(config);

        let features = make_features(1, 10000, 100, -0.5);
        // best_bid = 10000 - 50 = 9950
        let action = lt.on_features(&features, 1_000_000_000, 9950, 10050);

        match action {
            StrategyAction::Take(order) => {
                // Price should be below best_bid due to aggression
                assert!(order.price < 9950,
                    "Price {} should be below best_bid 9950", order.price);
            }
            _ => panic!("Expected Take action"),
        }
    }

    // ==================== Fill and Reset Tests ====================

    #[test]
    fn test_on_fill_reduces_cooldown() {
        let config = LiquidityTakerConfig::new(1)
            .with_min_interval_ns(100_000_000)
            .with_cooldown_factor(2.0);
        let mut lt = LiquidityTaker::new(config);

        // Increase cooldown
        lt.effective_interval_ns = 400_000_000;

        // Fill should reduce cooldown
        lt.on_fill();
        assert_eq!(lt.effective_interval_ns, 200_000_000);

        // But not below minimum
        lt.on_fill();
        lt.on_fill();
        lt.on_fill();
        assert!(lt.effective_interval_ns >= 100_000_000);
    }

    #[test]
    fn test_reset() {
        let mut lt = LiquidityTaker::for_ticker(1);

        let features = make_features(1, 10000, 100, 0.5);
        lt.on_features_simple(&features, 1_000_000_000);

        assert!(lt.orders_sent() > 0);
        assert!(lt.last_order_time_ns > 0);

        lt.reset();

        assert_eq!(lt.orders_sent(), 0);
        assert_eq!(lt.last_order_time_ns, 0);
        assert_eq!(lt.effective_interval_ns, lt.config.min_order_interval_ns);
    }

    #[test]
    fn test_orders_sent_counter() {
        let config = LiquidityTakerConfig::new(1)
            .with_threshold(0.3)
            .with_min_interval_ns(1); // Very short for testing
        let mut lt = LiquidityTaker::new(config);

        let features = make_features(1, 10000, 100, 0.5);

        assert_eq!(lt.orders_sent(), 0);

        lt.on_features_simple(&features, 1_000_000);
        assert_eq!(lt.orders_sent(), 1);

        lt.on_features_simple(&features, 1_000_000_000);
        assert_eq!(lt.orders_sent(), 2);
    }
}
