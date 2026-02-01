//! Market maker strategy for providing liquidity.
//!
//! The market maker calculates bid and ask prices around the fair value
//! (from the FeatureEngine) and generates quote updates when market conditions
//! change. It aims to profit from the bid-ask spread while managing inventory risk.

use common::{Price, Qty, TickerId};
use crate::features::TickerFeatures;
use super::{OrderRequest, QuotePair, StrategyAction};

/// Configuration parameters for the market maker strategy.
#[derive(Debug, Clone, Copy)]
pub struct MarketMakerConfig {
    /// The ticker this strategy trades.
    pub ticker_id: TickerId,
    /// Half-spread in price units (bid = fair_value - half_spread).
    /// Total spread will be 2 * half_spread.
    pub half_spread: Price,
    /// Minimum spread to quote (won't quote tighter than this).
    pub min_spread: Price,
    /// Base quantity to quote on each side.
    pub base_qty: Qty,
    /// Maximum quantity to quote on each side.
    pub max_qty: Qty,
    /// Price movement threshold to trigger quote update (in price units).
    /// Quotes are only updated when fair value moves by more than this amount.
    pub price_update_threshold: Price,
    /// Position skew factor: reduce qty on the side that increases position.
    /// 0.0 = no skew, 1.0 = full skew based on position.
    pub position_skew_factor: f64,
    /// Maximum position before stopping one-sided quoting.
    pub max_position: i64,
}

impl Default for MarketMakerConfig {
    fn default() -> Self {
        Self {
            ticker_id: 0,
            half_spread: 50,       // 50 cents = $0.50 half-spread
            min_spread: 20,        // 20 cents = $0.20 minimum half-spread
            base_qty: 100,         // 100 shares base
            max_qty: 500,          // 500 shares max
            price_update_threshold: 10, // Update quotes when price moves 10 cents
            position_skew_factor: 0.5,  // 50% position skew
            max_position: 1000,    // Stop adding to position at 1000 shares
        }
    }
}

impl MarketMakerConfig {
    /// Creates a new market maker config for a specific ticker.
    pub fn new(ticker_id: TickerId) -> Self {
        Self {
            ticker_id,
            ..Default::default()
        }
    }

    /// Builder method to set half spread.
    pub fn with_half_spread(mut self, half_spread: Price) -> Self {
        self.half_spread = half_spread;
        self
    }

    /// Builder method to set minimum spread.
    pub fn with_min_spread(mut self, min_spread: Price) -> Self {
        self.min_spread = min_spread;
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

    /// Builder method to set price update threshold.
    pub fn with_price_threshold(mut self, threshold: Price) -> Self {
        self.price_update_threshold = threshold;
        self
    }

    /// Builder method to set position skew factor.
    pub fn with_position_skew(mut self, factor: f64) -> Self {
        self.position_skew_factor = factor.clamp(0.0, 1.0);
        self
    }

    /// Builder method to set max position.
    pub fn with_max_position(mut self, max_position: i64) -> Self {
        self.max_position = max_position;
        self
    }
}

/// Market maker strategy state for a single ticker.
///
/// Maintains the last quoted prices and generates new quotes when market
/// conditions change significantly.
pub struct MarketMaker {
    /// Strategy configuration.
    config: MarketMakerConfig,
    /// Last quoted bid price.
    last_bid_price: Price,
    /// Last quoted ask price.
    last_ask_price: Price,
    /// Current position (tracked externally, updated via set_position).
    current_position: i64,
    /// Whether the strategy is active.
    active: bool,
}

impl MarketMaker {
    /// Creates a new market maker with the given configuration.
    pub fn new(config: MarketMakerConfig) -> Self {
        Self {
            config,
            last_bid_price: 0,
            last_ask_price: 0,
            current_position: 0,
            active: true,
        }
    }

    /// Creates a market maker with default config for a ticker.
    pub fn for_ticker(ticker_id: TickerId) -> Self {
        Self::new(MarketMakerConfig::new(ticker_id))
    }

    /// Returns a reference to the configuration.
    #[inline]
    pub fn config(&self) -> &MarketMakerConfig {
        &self.config
    }

    /// Returns a mutable reference to the configuration.
    #[inline]
    pub fn config_mut(&mut self) -> &mut MarketMakerConfig {
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

    /// Returns the last quoted bid price.
    #[inline]
    pub fn last_bid_price(&self) -> Price {
        self.last_bid_price
    }

    /// Returns the last quoted ask price.
    #[inline]
    pub fn last_ask_price(&self) -> Price {
        self.last_ask_price
    }

    /// Processes features and generates quote updates if needed.
    ///
    /// This is the main strategy entry point. It should be called whenever
    /// new market data is processed and features are updated.
    ///
    /// # Arguments
    /// * `features` - The current ticker features from the feature engine
    ///
    /// # Returns
    /// A `StrategyAction` indicating what action to take (if any)
    pub fn on_features(&mut self, features: &TickerFeatures) -> StrategyAction {
        // Check if strategy is active
        if !self.active {
            return StrategyAction::None;
        }

        // Check if features are valid
        if !features.is_valid() {
            return StrategyAction::None;
        }

        // Calculate new quote prices
        let (bid_price, ask_price) = self.calculate_quotes(features);

        // Check if we need to update quotes
        if self.should_update_quotes(bid_price, ask_price) {
            // Calculate quantities with position skew
            let (bid_qty, ask_qty) = self.calculate_quantities();

            // Update last quoted prices
            self.last_bid_price = bid_price;
            self.last_ask_price = ask_price;

            // Generate quote pair
            let quote_pair = self.build_quote_pair(bid_price, bid_qty, ask_price, ask_qty);
            StrategyAction::Quote(quote_pair)
        } else {
            StrategyAction::None
        }
    }

    /// Calculates bid and ask prices based on fair value and spread settings.
    ///
    /// The bid is placed at fair_value - half_spread and the ask at
    /// fair_value + half_spread, adjusted by the order book imbalance.
    fn calculate_quotes(&self, features: &TickerFeatures) -> (Price, Price) {
        let fair_value = features.fair_value;

        // Adjust spread based on market conditions
        // Widen spread when imbalance is high (more uncertainty)
        let imbalance_adjustment = (features.imbalance.abs() * self.config.half_spread as f64 * 0.5) as Price;
        let adjusted_half_spread = (self.config.half_spread + imbalance_adjustment)
            .max(self.config.min_spread);

        // Skew quotes based on order book imbalance
        // Positive imbalance (more bids) -> lower our bid, raise our ask
        // This helps avoid adverse selection
        let imbalance_skew = (features.imbalance * adjusted_half_spread as f64 * 0.2) as Price;

        let bid_price = fair_value - adjusted_half_spread - imbalance_skew;
        let ask_price = fair_value + adjusted_half_spread - imbalance_skew;

        // Ensure bid < ask
        let bid_price = bid_price.min(ask_price - 1);

        (bid_price, ask_price)
    }

    /// Calculates quote quantities based on position and skew settings.
    ///
    /// When we have a long position, we reduce bid quantity and increase ask quantity
    /// to help reduce the position. The opposite for short positions.
    fn calculate_quantities(&self) -> (Qty, Qty) {
        let base = self.config.base_qty as f64;
        let max = self.config.max_qty;
        let max_pos = self.config.max_position as f64;
        let skew = self.config.position_skew_factor;

        // Normalize position to [-1, 1] range
        let position_ratio = if max_pos > 0.0 {
            (self.current_position as f64 / max_pos).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        // Calculate skewed quantities
        // Long position (positive ratio) -> reduce bid qty, increase ask qty
        // Short position (negative ratio) -> increase bid qty, reduce ask qty
        let bid_factor = 1.0 - (skew * position_ratio).max(0.0);
        let ask_factor = 1.0 + (skew * position_ratio).min(0.0);

        let bid_qty = ((base * bid_factor) as Qty).clamp(1, max);
        let ask_qty = ((base * ask_factor) as Qty).clamp(1, max);

        // If at max position, stop quoting on the side that increases position
        let bid_qty = if self.current_position >= self.config.max_position {
            0 // Don't buy more if at max long
        } else {
            bid_qty
        };

        let ask_qty = if self.current_position <= -self.config.max_position {
            0 // Don't sell more if at max short
        } else {
            ask_qty
        };

        (bid_qty, ask_qty)
    }

    /// Determines if quotes should be updated based on price movement.
    fn should_update_quotes(&self, new_bid: Price, new_ask: Price) -> bool {
        // Always update if we haven't quoted yet
        if self.last_bid_price == 0 || self.last_ask_price == 0 {
            return true;
        }

        // Update if price moved by more than threshold
        let bid_moved = (new_bid - self.last_bid_price).abs() >= self.config.price_update_threshold;
        let ask_moved = (new_ask - self.last_ask_price).abs() >= self.config.price_update_threshold;

        bid_moved || ask_moved
    }

    /// Builds a QuotePair from the calculated prices and quantities.
    fn build_quote_pair(
        &self,
        bid_price: Price,
        bid_qty: Qty,
        ask_price: Price,
        ask_qty: Qty,
    ) -> QuotePair {
        let ticker_id = self.config.ticker_id;

        let bid = if bid_qty > 0 {
            Some(OrderRequest::buy(ticker_id, bid_price, bid_qty))
        } else {
            None
        };

        let ask = if ask_qty > 0 {
            Some(OrderRequest::sell(ticker_id, ask_price, ask_qty))
        } else {
            None
        };

        QuotePair { bid, ask }
    }

    /// Resets the strategy state (e.g., after a disconnect).
    pub fn reset(&mut self) {
        self.last_bid_price = 0;
        self.last_ask_price = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_features(ticker_id: TickerId, fair_value: Price, spread: Price, imbalance: f64) -> TickerFeatures {
        TickerFeatures {
            ticker_id,
            fair_value,
            spread,
            mid_price: fair_value,
            imbalance,
            trade_signal: 0.0,
        }
    }

    // ==================== Config Tests ====================

    #[test]
    fn test_config_default() {
        let config = MarketMakerConfig::default();
        assert_eq!(config.half_spread, 50);
        assert_eq!(config.min_spread, 20);
        assert_eq!(config.base_qty, 100);
        assert_eq!(config.max_qty, 500);
    }

    #[test]
    fn test_config_builder() {
        let config = MarketMakerConfig::new(1)
            .with_half_spread(100)
            .with_min_spread(50)
            .with_base_qty(200)
            .with_max_qty(1000)
            .with_position_skew(0.8)
            .with_max_position(5000);

        assert_eq!(config.ticker_id, 1);
        assert_eq!(config.half_spread, 100);
        assert_eq!(config.min_spread, 50);
        assert_eq!(config.base_qty, 200);
        assert_eq!(config.max_qty, 1000);
        assert!((config.position_skew_factor - 0.8).abs() < f64::EPSILON);
        assert_eq!(config.max_position, 5000);
    }

    #[test]
    fn test_config_position_skew_clamped() {
        let config = MarketMakerConfig::new(1).with_position_skew(1.5);
        assert!((config.position_skew_factor - 1.0).abs() < f64::EPSILON);

        let config = MarketMakerConfig::new(1).with_position_skew(-0.5);
        assert!(config.position_skew_factor.abs() < f64::EPSILON);
    }

    // ==================== Market Maker Construction Tests ====================

    #[test]
    fn test_market_maker_new() {
        let mm = MarketMaker::for_ticker(1);
        assert_eq!(mm.config.ticker_id, 1);
        assert_eq!(mm.last_bid_price, 0);
        assert_eq!(mm.last_ask_price, 0);
        assert_eq!(mm.current_position, 0);
        assert!(mm.is_active());
    }

    #[test]
    fn test_market_maker_activate_deactivate() {
        let mut mm = MarketMaker::for_ticker(1);
        assert!(mm.is_active());

        mm.deactivate();
        assert!(!mm.is_active());

        mm.activate();
        assert!(mm.is_active());
    }

    #[test]
    fn test_market_maker_set_position() {
        let mut mm = MarketMaker::for_ticker(1);
        assert_eq!(mm.position(), 0);

        mm.set_position(500);
        assert_eq!(mm.position(), 500);

        mm.set_position(-300);
        assert_eq!(mm.position(), -300);
    }

    // ==================== Quote Calculation Tests ====================

    #[test]
    fn test_on_features_generates_quotes() {
        let mut mm = MarketMaker::for_ticker(1);
        let features = make_features(1, 10000, 100, 0.0);

        let action = mm.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                assert!(pair.is_two_sided());
                let bid = pair.bid.unwrap();
                let ask = pair.ask.unwrap();

                // Bid should be below fair value
                assert!(bid.price < 10000);
                // Ask should be above fair value
                assert!(ask.price > 10000);
                // Both should have the base quantity
                assert_eq!(bid.qty, 100);
                assert_eq!(ask.qty, 100);
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_on_features_inactive_returns_none() {
        let mut mm = MarketMaker::for_ticker(1);
        mm.deactivate();

        let features = make_features(1, 10000, 100, 0.0);
        let action = mm.on_features(&features);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_on_features_invalid_features_returns_none() {
        let mut mm = MarketMaker::for_ticker(1);

        // Invalid features (mid_price = 0)
        let features = TickerFeatures::new(1);
        let action = mm.on_features(&features);

        assert!(matches!(action, StrategyAction::None));
    }

    #[test]
    fn test_quote_spread() {
        let config = MarketMakerConfig::new(1)
            .with_half_spread(50)
            .with_min_spread(20);
        let mut mm = MarketMaker::new(config);

        let features = make_features(1, 10000, 100, 0.0);
        let action = mm.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                let bid = pair.bid.unwrap();
                let ask = pair.ask.unwrap();

                // Spread should be approximately 2 * half_spread
                let spread = ask.price - bid.price;
                assert!(spread >= 100, "Spread {} should be at least 100", spread);
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_quote_no_update_within_threshold() {
        let config = MarketMakerConfig::new(1).with_price_threshold(10);
        let mut mm = MarketMaker::new(config);

        // First quote
        let features1 = make_features(1, 10000, 100, 0.0);
        let action1 = mm.on_features(&features1);
        assert!(matches!(action1, StrategyAction::Quote(_)));

        // Small price change - should not update
        let features2 = make_features(1, 10005, 100, 0.0);
        let action2 = mm.on_features(&features2);
        assert!(matches!(action2, StrategyAction::None));

        // Large price change - should update
        let features3 = make_features(1, 10050, 100, 0.0);
        let action3 = mm.on_features(&features3);
        assert!(matches!(action3, StrategyAction::Quote(_)));
    }

    // ==================== Position Skew Tests ====================

    #[test]
    fn test_position_skew_long_position() {
        let config = MarketMakerConfig::new(1)
            .with_base_qty(100)
            .with_position_skew(0.5)
            .with_max_position(1000);
        let mut mm = MarketMaker::new(config);

        // Set a long position
        mm.set_position(500); // 50% of max

        let features = make_features(1, 10000, 100, 0.0);
        let action = mm.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                let bid = pair.bid.unwrap();
                let ask = pair.ask.unwrap();

                // With long position, bid qty should be reduced
                assert!(bid.qty < 100, "Bid qty {} should be less than base 100", bid.qty);
                // Ask qty should be unchanged or higher (to reduce position)
                assert!(ask.qty >= 100, "Ask qty {} should be at least 100", ask.qty);
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_position_skew_short_position() {
        let config = MarketMakerConfig::new(1)
            .with_base_qty(100)
            .with_position_skew(0.5)
            .with_max_position(1000);
        let mut mm = MarketMaker::new(config);

        // Set a short position
        mm.set_position(-500); // -50% of max

        let features = make_features(1, 10000, 100, 0.0);
        let action = mm.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                let bid = pair.bid.unwrap();
                let ask = pair.ask.unwrap();

                // With short position, ask qty should be reduced
                assert!(ask.qty < 100, "Ask qty {} should be less than base 100", ask.qty);
                // Bid qty should be unchanged or higher (to reduce position)
                assert!(bid.qty >= 100, "Bid qty {} should be at least 100", bid.qty);
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_max_position_stops_quoting() {
        let config = MarketMakerConfig::new(1)
            .with_base_qty(100)
            .with_max_position(1000);
        let mut mm = MarketMaker::new(config);

        // At max long position
        mm.set_position(1000);

        let features = make_features(1, 10000, 100, 0.0);
        let action = mm.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                // Should not have bid (can't buy more)
                assert!(pair.bid.is_none(), "Should not quote bid at max long position");
                // Should have ask (can still sell)
                assert!(pair.ask.is_some(), "Should still quote ask at max long position");
            }
            _ => panic!("Expected Quote action"),
        }
    }

    #[test]
    fn test_max_short_position_stops_selling() {
        let config = MarketMakerConfig::new(1)
            .with_base_qty(100)
            .with_max_position(1000);
        let mut mm = MarketMaker::new(config);

        // At max short position
        mm.set_position(-1000);

        let features = make_features(1, 10000, 100, 0.0);
        let action = mm.on_features(&features);

        match action {
            StrategyAction::Quote(pair) => {
                // Should have bid (can buy to cover)
                assert!(pair.bid.is_some(), "Should still quote bid at max short position");
                // Should not have ask (can't sell more)
                assert!(pair.ask.is_none(), "Should not quote ask at max short position");
            }
            _ => panic!("Expected Quote action"),
        }
    }

    // ==================== Imbalance Adjustment Tests ====================

    #[test]
    fn test_imbalance_widens_spread() {
        let config = MarketMakerConfig::new(1).with_half_spread(50);
        let mut mm1 = MarketMaker::new(config);
        let mut mm2 = MarketMaker::new(config);

        // Zero imbalance
        let features1 = make_features(1, 10000, 100, 0.0);
        let action1 = mm1.on_features(&features1);

        // High imbalance
        let features2 = make_features(1, 10000, 100, 0.8);
        let action2 = mm2.on_features(&features2);

        let spread1 = match action1 {
            StrategyAction::Quote(pair) => pair.ask.unwrap().price - pair.bid.unwrap().price,
            _ => panic!("Expected Quote"),
        };

        let spread2 = match action2 {
            StrategyAction::Quote(pair) => pair.ask.unwrap().price - pair.bid.unwrap().price,
            _ => panic!("Expected Quote"),
        };

        assert!(spread2 >= spread1, "Higher imbalance should result in wider spread");
    }

    // ==================== Reset Tests ====================

    #[test]
    fn test_reset() {
        let mut mm = MarketMaker::for_ticker(1);

        let features = make_features(1, 10000, 100, 0.0);
        mm.on_features(&features);

        assert!(mm.last_bid_price > 0);
        assert!(mm.last_ask_price > 0);

        mm.reset();

        assert_eq!(mm.last_bid_price, 0);
        assert_eq!(mm.last_ask_price, 0);
    }

    #[test]
    fn test_quotes_after_reset() {
        let mut mm = MarketMaker::for_ticker(1);

        // Initial quote
        let features = make_features(1, 10000, 100, 0.0);
        mm.on_features(&features);

        mm.reset();

        // Should generate new quotes after reset even with same price
        let action = mm.on_features(&features);
        assert!(matches!(action, StrategyAction::Quote(_)));
    }
}
