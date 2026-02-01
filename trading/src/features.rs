//! Signal generation and feature extraction for trading strategies.
//!
//! This module provides a feature engine that computes trading signals from
//! market data. It calculates fair value estimates, spread metrics, order book
//! imbalance, and generates trade signals based on these features.

use common::{Price, TickerId};
use crate::market_data::BBO;
use std::collections::HashMap;

/// Trading features computed for a single ticker.
///
/// Contains derived metrics from market data that can be used by trading
/// strategies to make decisions.
#[derive(Debug, Clone, Default)]
pub struct TickerFeatures {
    /// The ticker this feature set applies to.
    pub ticker_id: TickerId,
    /// Estimated fair value using EMA smoothing of mid prices.
    pub fair_value: Price,
    /// Current bid-ask spread.
    pub spread: Price,
    /// Current mid price ((bid + ask) / 2).
    pub mid_price: Price,
    /// Order book imbalance: -1.0 to 1.0, positive = more bids (buy pressure).
    pub imbalance: f64,
    /// Trade signal: -1.0 to 1.0, positive = buy signal.
    pub trade_signal: f64,
}

impl TickerFeatures {
    /// Creates new features for a ticker with default values.
    pub fn new(ticker_id: TickerId) -> Self {
        Self {
            ticker_id,
            fair_value: 0,
            spread: 0,
            mid_price: 0,
            imbalance: 0.0,
            trade_signal: 0.0,
        }
    }

    /// Returns true if the features have been initialized with valid data.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.mid_price > 0 && self.fair_value > 0
    }
}

/// Feature engine for computing trading signals from market data.
///
/// Maintains feature state for multiple tickers and updates them as new
/// market data arrives. Uses exponential moving average (EMA) for fair
/// value estimation to smooth out short-term price fluctuations.
pub struct FeatureEngine {
    /// Per-ticker feature state.
    features: HashMap<TickerId, TickerFeatures>,
    /// EMA smoothing factor for fair value calculation (0.0 to 1.0).
    /// Higher values give more weight to recent observations.
    fair_value_alpha: f64,
}

impl Default for FeatureEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureEngine {
    /// Default EMA alpha for fair value calculation.
    /// 0.1 gives ~90% weight to historical values, providing good smoothing.
    const DEFAULT_FAIR_VALUE_ALPHA: f64 = 0.1;

    /// Creates a new FeatureEngine with default parameters.
    pub fn new() -> Self {
        Self {
            features: HashMap::new(),
            fair_value_alpha: Self::DEFAULT_FAIR_VALUE_ALPHA,
        }
    }

    /// Creates a new FeatureEngine with a custom EMA alpha.
    ///
    /// # Arguments
    /// * `fair_value_alpha` - EMA smoothing factor (0.0 to 1.0).
    ///   Higher values make fair value more responsive to recent prices.
    pub fn with_alpha(fair_value_alpha: f64) -> Self {
        Self {
            features: HashMap::new(),
            fair_value_alpha: fair_value_alpha.clamp(0.0, 1.0),
        }
    }

    /// Processes a BBO update and recalculates features for the ticker.
    ///
    /// This method:
    /// 1. Calculates the mid price from bid/ask
    /// 2. Updates fair value using EMA
    /// 3. Calculates spread and order book imbalance
    /// 4. Generates a trade signal based on fair value vs mid price
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker that received the update
    /// * `bbo` - The updated best bid/offer
    pub fn on_bbo_update(&mut self, ticker_id: TickerId, bbo: &BBO) {
        // Only process valid BBOs with both bid and ask
        if !bbo.is_valid() {
            return;
        }

        // Get or create feature entry for this ticker
        let features = self.features
            .entry(ticker_id)
            .or_insert_with(|| TickerFeatures::new(ticker_id));

        // 1. Calculate mid price
        let mid_price = (bbo.bid_price + bbo.ask_price) / 2;
        features.mid_price = mid_price;

        // 2. Update fair value using EMA
        // fair_value = alpha * mid_price + (1 - alpha) * fair_value
        if features.fair_value == 0 {
            // First update - initialize fair value to current mid
            features.fair_value = mid_price;
        } else {
            // EMA update: new_value = alpha * observation + (1 - alpha) * old_value
            let mid_f64 = mid_price as f64;
            let fv_f64 = features.fair_value as f64;
            let new_fv = self.fair_value_alpha * mid_f64 + (1.0 - self.fair_value_alpha) * fv_f64;
            features.fair_value = new_fv.round() as Price;
        }

        // 3. Calculate spread
        features.spread = bbo.ask_price - bbo.bid_price;

        // 4. Calculate order book imbalance
        features.imbalance = Self::calculate_imbalance(bbo);

        // 5. Generate trade signal
        features.trade_signal = Self::calculate_trade_signal_from_features(features);
    }

    /// Returns the current features for a ticker.
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker to look up
    ///
    /// # Returns
    /// - `Some(&TickerFeatures)` if features exist for this ticker
    /// - `None` if no data has been processed for this ticker
    #[inline]
    pub fn get_features(&self, ticker_id: TickerId) -> Option<&TickerFeatures> {
        self.features.get(&ticker_id)
    }

    /// Calculates order book imbalance from BBO quantities.
    ///
    /// Imbalance = (bid_qty - ask_qty) / (bid_qty + ask_qty)
    ///
    /// Returns a value between -1.0 and 1.0:
    /// - Positive values indicate more bid quantity (buying pressure)
    /// - Negative values indicate more ask quantity (selling pressure)
    /// - Zero indicates balanced order book
    ///
    /// # Arguments
    /// * `bbo` - The best bid/offer to calculate imbalance from
    ///
    /// # Returns
    /// Imbalance value from -1.0 to 1.0
    pub fn calculate_imbalance(bbo: &BBO) -> f64 {
        let bid_qty = bbo.bid_qty as f64;
        let ask_qty = bbo.ask_qty as f64;
        let total_qty = bid_qty + ask_qty;

        if total_qty == 0.0 {
            return 0.0;
        }

        (bid_qty - ask_qty) / total_qty
    }

    /// Calculates a trade signal for a ticker based on fair value deviation.
    ///
    /// The signal is based on the difference between fair value and mid price:
    /// - If fair value > mid price: positive signal (buy opportunity)
    /// - If fair value < mid price: negative signal (sell opportunity)
    ///
    /// The signal is normalized by the spread to give a value between -1.0 and 1.0.
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker to calculate signal for
    ///
    /// # Returns
    /// Trade signal from -1.0 to 1.0, or 0.0 if no features exist
    pub fn calculate_trade_signal(&self, ticker_id: TickerId) -> f64 {
        match self.features.get(&ticker_id) {
            Some(features) => Self::calculate_trade_signal_from_features(features),
            None => 0.0,
        }
    }

    /// Internal helper to calculate trade signal from features.
    ///
    /// Signal combines:
    /// 1. Fair value deviation: (fair_value - mid_price) / spread
    /// 2. Order book imbalance
    ///
    /// Weighted combination with 70% weight on fair value deviation
    /// and 30% weight on imbalance.
    fn calculate_trade_signal_from_features(features: &TickerFeatures) -> f64 {
        if !features.is_valid() || features.spread <= 0 {
            return 0.0;
        }

        // Fair value deviation signal
        // Positive when fair value > mid price (undervalued, buy signal)
        let fv_deviation = (features.fair_value - features.mid_price) as f64;
        let spread_f64 = features.spread as f64;

        // Normalize by spread, clamp to [-1, 1]
        let fv_signal = (fv_deviation / spread_f64).clamp(-1.0, 1.0);

        // Combine with imbalance (imbalance already in [-1, 1])
        // Weight: 70% fair value signal, 30% imbalance
        let combined_signal = 0.7 * fv_signal + 0.3 * features.imbalance;

        // Final clamp to ensure [-1, 1] range
        combined_signal.clamp(-1.0, 1.0)
    }

    /// Returns an iterator over all ticker features.
    #[inline]
    pub fn iter_features(&self) -> impl Iterator<Item = (&TickerId, &TickerFeatures)> {
        self.features.iter()
    }

    /// Returns the number of tickers with features.
    #[inline]
    pub fn ticker_count(&self) -> usize {
        self.features.len()
    }

    /// Pre-allocates feature entries for the given tickers.
    ///
    /// This can reduce allocation during runtime.
    pub fn reserve_tickers(&mut self, tickers: &[TickerId]) {
        for &ticker_id in tickers {
            self.features
                .entry(ticker_id)
                .or_insert_with(|| TickerFeatures::new(ticker_id));
        }
    }

    /// Clears all feature data.
    pub fn clear(&mut self) {
        self.features.clear();
    }

    /// Returns the current fair value alpha (EMA smoothing factor).
    #[inline]
    pub fn fair_value_alpha(&self) -> f64 {
        self.fair_value_alpha
    }

    /// Sets a new fair value alpha (EMA smoothing factor).
    ///
    /// # Arguments
    /// * `alpha` - New alpha value, will be clamped to [0.0, 1.0]
    pub fn set_fair_value_alpha(&mut self, alpha: f64) {
        self.fair_value_alpha = alpha.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Qty, INVALID_PRICE};

    fn make_bbo(bid_price: Price, bid_qty: Qty, ask_price: Price, ask_qty: Qty) -> BBO {
        BBO {
            bid_price,
            bid_qty,
            ask_price,
            ask_qty,
        }
    }

    #[test]
    fn test_ticker_features_new() {
        let features = TickerFeatures::new(42);
        assert_eq!(features.ticker_id, 42);
        assert_eq!(features.fair_value, 0);
        assert_eq!(features.spread, 0);
        assert_eq!(features.mid_price, 0);
        assert_eq!(features.imbalance, 0.0);
        assert_eq!(features.trade_signal, 0.0);
        assert!(!features.is_valid());
    }

    #[test]
    fn test_feature_engine_new() {
        let engine = FeatureEngine::new();
        assert_eq!(engine.ticker_count(), 0);
        assert!((engine.fair_value_alpha() - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_feature_engine_with_alpha() {
        let engine = FeatureEngine::with_alpha(0.5);
        assert!((engine.fair_value_alpha() - 0.5).abs() < f64::EPSILON);

        // Test clamping
        let engine_high = FeatureEngine::with_alpha(1.5);
        assert!((engine_high.fair_value_alpha() - 1.0).abs() < f64::EPSILON);

        let engine_low = FeatureEngine::with_alpha(-0.5);
        assert!(engine_low.fair_value_alpha().abs() < f64::EPSILON);
    }

    #[test]
    fn test_calculate_imbalance() {
        // Balanced book
        let bbo = make_bbo(100, 50, 102, 50);
        let imbalance = FeatureEngine::calculate_imbalance(&bbo);
        assert!(imbalance.abs() < f64::EPSILON);

        // More bids (positive imbalance)
        let bbo = make_bbo(100, 75, 102, 25);
        let imbalance = FeatureEngine::calculate_imbalance(&bbo);
        assert!((imbalance - 0.5).abs() < f64::EPSILON);

        // More asks (negative imbalance)
        let bbo = make_bbo(100, 25, 102, 75);
        let imbalance = FeatureEngine::calculate_imbalance(&bbo);
        assert!((imbalance - (-0.5)).abs() < f64::EPSILON);

        // All bids, no asks
        let bbo = make_bbo(100, 100, 102, 0);
        let imbalance = FeatureEngine::calculate_imbalance(&bbo);
        assert!((imbalance - 1.0).abs() < f64::EPSILON);

        // No quantity at all
        let bbo = make_bbo(100, 0, 102, 0);
        let imbalance = FeatureEngine::calculate_imbalance(&bbo);
        assert!(imbalance.abs() < f64::EPSILON);
    }

    #[test]
    fn test_on_bbo_update_first_update() {
        let mut engine = FeatureEngine::new();
        let ticker_id: TickerId = 1;
        let bbo = make_bbo(100, 50, 102, 50);

        engine.on_bbo_update(ticker_id, &bbo);

        let features = engine.get_features(ticker_id).expect("Features should exist");
        assert_eq!(features.ticker_id, ticker_id);
        assert_eq!(features.mid_price, 101); // (100 + 102) / 2
        assert_eq!(features.fair_value, 101); // First update, equals mid
        assert_eq!(features.spread, 2); // 102 - 100
        assert!(features.imbalance.abs() < f64::EPSILON); // Balanced
        assert!(features.is_valid());
    }

    #[test]
    fn test_on_bbo_update_ema() {
        let mut engine = FeatureEngine::with_alpha(0.5); // 50% weight on new values
        let ticker_id: TickerId = 1;

        // First update: mid = 100, fair_value = 100
        let bbo1 = make_bbo(99, 50, 101, 50);
        engine.on_bbo_update(ticker_id, &bbo1);
        assert_eq!(engine.get_features(ticker_id).unwrap().fair_value, 100);

        // Second update: mid = 110
        // fair_value = 0.5 * 110 + 0.5 * 100 = 105
        let bbo2 = make_bbo(109, 50, 111, 50);
        engine.on_bbo_update(ticker_id, &bbo2);
        assert_eq!(engine.get_features(ticker_id).unwrap().fair_value, 105);

        // Third update: mid = 110 again
        // fair_value = 0.5 * 110 + 0.5 * 105 = 107.5 -> 108 (rounded)
        engine.on_bbo_update(ticker_id, &bbo2);
        assert_eq!(engine.get_features(ticker_id).unwrap().fair_value, 108);
    }

    #[test]
    fn test_on_bbo_update_invalid_bbo() {
        let mut engine = FeatureEngine::new();
        let ticker_id: TickerId = 1;

        // Invalid BBO (no bid)
        let bbo = BBO {
            bid_price: INVALID_PRICE,
            bid_qty: 0,
            ask_price: 102,
            ask_qty: 50,
        };

        engine.on_bbo_update(ticker_id, &bbo);
        assert!(engine.get_features(ticker_id).is_none());
    }

    #[test]
    fn test_trade_signal_fair_value_above_mid() {
        let mut engine = FeatureEngine::with_alpha(0.1);
        let ticker_id: TickerId = 1;

        // Initialize with high price
        let bbo_high = make_bbo(109, 50, 111, 50);
        for _ in 0..20 {
            engine.on_bbo_update(ticker_id, &bbo_high);
        }

        // Now price drops - fair value > mid price = buy signal
        let bbo_low = make_bbo(99, 50, 101, 50);
        engine.on_bbo_update(ticker_id, &bbo_low);

        let features = engine.get_features(ticker_id).unwrap();
        assert!(features.trade_signal > 0.0, "Should have positive (buy) signal");
    }

    #[test]
    fn test_trade_signal_fair_value_below_mid() {
        let mut engine = FeatureEngine::with_alpha(0.1);
        let ticker_id: TickerId = 1;

        // Initialize with low price
        let bbo_low = make_bbo(99, 50, 101, 50);
        for _ in 0..20 {
            engine.on_bbo_update(ticker_id, &bbo_low);
        }

        // Now price rises - fair value < mid price = sell signal
        let bbo_high = make_bbo(109, 50, 111, 50);
        engine.on_bbo_update(ticker_id, &bbo_high);

        let features = engine.get_features(ticker_id).unwrap();
        assert!(features.trade_signal < 0.0, "Should have negative (sell) signal");
    }

    #[test]
    fn test_trade_signal_with_imbalance() {
        let mut engine = FeatureEngine::with_alpha(1.0); // Use mid price directly as fair value
        let ticker_id: TickerId = 1;

        // Balanced fair value but heavy bid imbalance (need non-zero qty on both sides for valid BBO)
        let bbo = make_bbo(100, 90, 102, 10); // 90% bids, 10% asks
        engine.on_bbo_update(ticker_id, &bbo);

        let features = engine.get_features(ticker_id).unwrap();
        // With alpha=1.0, fair_value equals mid_price, so fv_signal = 0
        // Imbalance = (90 - 10) / (90 + 10) = 0.8
        // Signal = 0.7 * 0 + 0.3 * 0.8 = 0.24
        assert!((features.trade_signal - 0.24).abs() < 0.01);
    }

    #[test]
    fn test_calculate_trade_signal_no_features() {
        let engine = FeatureEngine::new();
        assert!(engine.calculate_trade_signal(999).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reserve_tickers() {
        let mut engine = FeatureEngine::new();
        engine.reserve_tickers(&[1, 2, 3]);

        assert_eq!(engine.ticker_count(), 3);
        assert!(engine.get_features(1).is_some());
        assert!(engine.get_features(2).is_some());
        assert!(engine.get_features(3).is_some());
        assert!(engine.get_features(4).is_none());
    }

    #[test]
    fn test_clear() {
        let mut engine = FeatureEngine::new();
        let bbo = make_bbo(100, 50, 102, 50);
        engine.on_bbo_update(1, &bbo);
        engine.on_bbo_update(2, &bbo);

        assert_eq!(engine.ticker_count(), 2);
        engine.clear();
        assert_eq!(engine.ticker_count(), 0);
    }

    #[test]
    fn test_iter_features() {
        let mut engine = FeatureEngine::new();
        let bbo = make_bbo(100, 50, 102, 50);
        engine.on_bbo_update(1, &bbo);
        engine.on_bbo_update(2, &bbo);

        let ticker_ids: Vec<_> = engine.iter_features().map(|(id, _)| *id).collect();
        assert_eq!(ticker_ids.len(), 2);
        assert!(ticker_ids.contains(&1));
        assert!(ticker_ids.contains(&2));
    }

    #[test]
    fn test_set_fair_value_alpha() {
        let mut engine = FeatureEngine::new();
        engine.set_fair_value_alpha(0.8);
        assert!((engine.fair_value_alpha() - 0.8).abs() < f64::EPSILON);

        // Test clamping
        engine.set_fair_value_alpha(2.0);
        assert!((engine.fair_value_alpha() - 1.0).abs() < f64::EPSILON);
    }
}
