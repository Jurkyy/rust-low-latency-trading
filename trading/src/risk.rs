// Risk management

use common::{Price, Qty, Side, TickerId};
use crate::position::Position;
use std::collections::HashMap;

/// Result of a pre-trade risk check
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskCheckResult {
    /// Order passes all risk checks
    Allowed,
    /// Order quantity exceeds maximum allowed order size
    OrderTooLarge,
    /// Resulting position would exceed maximum allowed position
    PositionTooLarge,
    /// Total loss exceeds maximum allowed loss
    LossTooLarge,
    /// Too many open orders
    OpenOrdersTooMany,
}

impl RiskCheckResult {
    /// Returns true if the order is allowed
    #[inline]
    pub fn is_allowed(&self) -> bool {
        matches!(self, RiskCheckResult::Allowed)
    }
}

/// Configurable risk limits for a ticker
#[derive(Debug, Clone, Copy)]
pub struct RiskLimits {
    /// Maximum quantity per single order
    pub max_order_qty: Qty,
    /// Maximum absolute position (long or short)
    pub max_position: i64,
    /// Maximum loss in cents (realized + unrealized)
    pub max_loss: i64,
    /// Maximum number of open orders
    pub max_open_orders: u32,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_order_qty: 1000,
            max_position: 10000,
            max_loss: 100000, // $1000 in cents
            max_open_orders: 100,
        }
    }
}

impl RiskLimits {
    /// Create new risk limits with specified values
    pub fn new(max_order_qty: Qty, max_position: i64, max_loss: i64, max_open_orders: u32) -> Self {
        Self {
            max_order_qty,
            max_position,
            max_loss,
            max_open_orders,
        }
    }
}

/// Risk manager for pre-trade validation and real-time position/P&L checks
pub struct RiskManager {
    /// Per-ticker risk limits
    limits: HashMap<TickerId, RiskLimits>,
    /// Default limits for tickers without specific limits
    default_limits: RiskLimits,
}

impl RiskManager {
    /// Creates a new risk manager with default limits
    pub fn new() -> Self {
        Self {
            limits: HashMap::new(),
            default_limits: RiskLimits::default(),
        }
    }

    /// Creates a new risk manager with custom default limits
    pub fn with_default_limits(default_limits: RiskLimits) -> Self {
        Self {
            limits: HashMap::new(),
            default_limits,
        }
    }

    /// Set risk limits for a specific ticker
    pub fn set_limits(&mut self, ticker_id: TickerId, limits: RiskLimits) {
        self.limits.insert(ticker_id, limits);
    }

    /// Get risk limits for a ticker (returns default if not set)
    pub fn get_limits(&self, ticker_id: TickerId) -> &RiskLimits {
        self.limits.get(&ticker_id).unwrap_or(&self.default_limits)
    }

    /// Remove ticker-specific limits (will use default)
    pub fn remove_limits(&mut self, ticker_id: TickerId) {
        self.limits.remove(&ticker_id);
    }

    /// Pre-trade risk check for a new order
    ///
    /// Validates:
    /// 1. Order quantity does not exceed max_order_qty
    /// 2. Resulting position (including pending orders) does not exceed max_position
    /// 3. Current P&L loss does not exceed max_loss
    ///
    /// Note: Open order count check should be done separately as it requires
    /// order book state not available in Position.
    pub fn check_order(
        &self,
        position: &Position,
        side: Side,
        qty: Qty,
        _price: Price,
    ) -> RiskCheckResult {
        let limits = self.get_limits(position.ticker_id);

        // Check 1: Order size limit
        if qty > limits.max_order_qty {
            return RiskCheckResult::OrderTooLarge;
        }

        // Check 2: Position limit (including pending orders)
        let projected_position = match side {
            Side::Buy => position.max_long_exposure() + qty as i64,
            Side::Sell => position.max_short_exposure() - qty as i64,
        };

        if projected_position.abs() > limits.max_position {
            return RiskCheckResult::PositionTooLarge;
        }

        // Check 3: Loss limit
        // Negative total_pnl means a loss
        if position.total_pnl() < -limits.max_loss {
            return RiskCheckResult::LossTooLarge;
        }

        RiskCheckResult::Allowed
    }

    /// Check if open order count is within limits
    pub fn check_open_orders(
        &self,
        ticker_id: TickerId,
        current_open_orders: u32,
    ) -> RiskCheckResult {
        let limits = self.get_limits(ticker_id);

        if current_open_orders >= limits.max_open_orders {
            return RiskCheckResult::OpenOrdersTooMany;
        }

        RiskCheckResult::Allowed
    }

    /// Real-time position check (can be called periodically or on updates)
    ///
    /// Validates:
    /// 1. Current position does not exceed max_position
    /// 2. Current P&L loss does not exceed max_loss
    pub fn check_position(&self, position: &Position) -> RiskCheckResult {
        let limits = self.get_limits(position.ticker_id);

        // Check position limit
        if position.position.abs() > limits.max_position {
            return RiskCheckResult::PositionTooLarge;
        }

        // Check loss limit
        if position.total_pnl() < -limits.max_loss {
            return RiskCheckResult::LossTooLarge;
        }

        RiskCheckResult::Allowed
    }

    /// Combined pre-trade check including open order count
    pub fn check_order_with_open_orders(
        &self,
        position: &Position,
        side: Side,
        qty: Qty,
        price: Price,
        current_open_orders: u32,
    ) -> RiskCheckResult {
        // First check open orders
        let open_orders_result = self.check_open_orders(position.ticker_id, current_open_orders);
        if !open_orders_result.is_allowed() {
            return open_orders_result;
        }

        // Then check the order itself
        self.check_order(position, side, qty, price)
    }
}

impl Default for RiskManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_position_with_state(
        ticker_id: TickerId,
        position: i64,
        open_buy_qty: Qty,
        open_sell_qty: Qty,
        realized_pnl: i64,
        unrealized_pnl: i64,
    ) -> Position {
        Position {
            ticker_id,
            position,
            open_buy_qty,
            open_sell_qty,
            volume_traded: 0,
            realized_pnl,
            unrealized_pnl,
            avg_open_price: 0,
            last_price: 0,
        }
    }

    // ==================== RiskCheckResult Tests ====================

    #[test]
    fn test_risk_check_result_is_allowed() {
        assert!(RiskCheckResult::Allowed.is_allowed());
        assert!(!RiskCheckResult::OrderTooLarge.is_allowed());
        assert!(!RiskCheckResult::PositionTooLarge.is_allowed());
        assert!(!RiskCheckResult::LossTooLarge.is_allowed());
        assert!(!RiskCheckResult::OpenOrdersTooMany.is_allowed());
    }

    // ==================== RiskLimits Tests ====================

    #[test]
    fn test_risk_limits_default() {
        let limits = RiskLimits::default();
        assert_eq!(limits.max_order_qty, 1000);
        assert_eq!(limits.max_position, 10000);
        assert_eq!(limits.max_loss, 100000);
        assert_eq!(limits.max_open_orders, 100);
    }

    #[test]
    fn test_risk_limits_new() {
        let limits = RiskLimits::new(500, 5000, 50000, 50);
        assert_eq!(limits.max_order_qty, 500);
        assert_eq!(limits.max_position, 5000);
        assert_eq!(limits.max_loss, 50000);
        assert_eq!(limits.max_open_orders, 50);
    }

    // ==================== RiskManager Construction Tests ====================

    #[test]
    fn test_risk_manager_new() {
        let rm = RiskManager::new();
        assert_eq!(rm.default_limits.max_order_qty, 1000);
        assert!(rm.limits.is_empty());
    }

    #[test]
    fn test_risk_manager_with_default_limits() {
        let custom_limits = RiskLimits::new(2000, 20000, 200000, 200);
        let rm = RiskManager::with_default_limits(custom_limits);
        assert_eq!(rm.default_limits.max_order_qty, 2000);
        assert_eq!(rm.default_limits.max_position, 20000);
    }

    #[test]
    fn test_risk_manager_set_and_get_limits() {
        let mut rm = RiskManager::new();
        let custom_limits = RiskLimits::new(500, 5000, 50000, 50);

        rm.set_limits(1, custom_limits);

        let limits = rm.get_limits(1);
        assert_eq!(limits.max_order_qty, 500);

        // Ticker without specific limits should return default
        let default_limits = rm.get_limits(2);
        assert_eq!(default_limits.max_order_qty, 1000);
    }

    #[test]
    fn test_risk_manager_remove_limits() {
        let mut rm = RiskManager::new();
        let custom_limits = RiskLimits::new(500, 5000, 50000, 50);

        rm.set_limits(1, custom_limits);
        assert_eq!(rm.get_limits(1).max_order_qty, 500);

        rm.remove_limits(1);
        assert_eq!(rm.get_limits(1).max_order_qty, 1000); // Back to default
    }

    // ==================== Order Size Check Tests ====================

    #[test]
    fn test_check_order_allowed() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        let result = rm.check_order(&position, Side::Buy, 100, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_too_large() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        // Default max_order_qty is 1000
        let result = rm.check_order(&position, Side::Buy, 1001, 5000);
        assert_eq!(result, RiskCheckResult::OrderTooLarge);
    }

    #[test]
    fn test_check_order_at_max_qty_allowed() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        // Exactly at limit should be allowed
        let result = rm.check_order(&position, Side::Buy, 1000, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    // ==================== Position Limit Check Tests ====================

    #[test]
    fn test_check_order_position_too_large_buy() {
        let rm = RiskManager::new();
        // Current position: 9500, no pending orders
        let position = create_position_with_state(1, 9500, 0, 0, 0, 0);

        // Buying 600 would result in position of 10100, exceeding 10000 limit
        let result = rm.check_order(&position, Side::Buy, 600, 5000);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);

        // Buying 500 would result in position of 10000, exactly at limit
        let result = rm.check_order(&position, Side::Buy, 500, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_position_too_large_sell_short() {
        let rm = RiskManager::new();
        // Current position: -9500 (short)
        let position = create_position_with_state(1, -9500, 0, 0, 0, 0);

        // Selling 600 would result in position of -10100, exceeding limit
        let result = rm.check_order(&position, Side::Sell, 600, 5000);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);

        // Selling 500 would result in position of -10000, exactly at limit
        let result = rm.check_order(&position, Side::Sell, 500, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_includes_pending_orders_buy() {
        let rm = RiskManager::new();
        // Current position: 9000, pending buy: 500
        // Max long exposure = 9000 + 500 = 9500
        let position = create_position_with_state(1, 9000, 500, 0, 0, 0);

        // New buy of 600 would make exposure 9500 + 600 = 10100, exceeding limit
        let result = rm.check_order(&position, Side::Buy, 600, 5000);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);

        // New buy of 500 would make exposure 9500 + 500 = 10000, at limit
        let result = rm.check_order(&position, Side::Buy, 500, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_includes_pending_orders_sell() {
        let rm = RiskManager::new();
        // Current position: -9000, pending sell: 500
        // Max short exposure = -9000 - 500 = -9500
        let position = create_position_with_state(1, -9000, 0, 500, 0, 0);

        // New sell of 600 would make exposure -9500 - 600 = -10100, exceeding limit
        let result = rm.check_order(&position, Side::Sell, 600, 5000);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);

        // New sell of 500 would make exposure -9500 - 500 = -10000, at limit
        let result = rm.check_order(&position, Side::Sell, 500, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_reducing_position_allowed() {
        let rm = RiskManager::new();
        // Even with large position, reducing it should be allowed
        let position = create_position_with_state(1, 15000, 0, 0, 0, 0);

        // Selling reduces long position
        let result = rm.check_order(&position, Side::Sell, 1000, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    // ==================== Loss Limit Check Tests ====================

    #[test]
    fn test_check_order_loss_too_large() {
        let rm = RiskManager::new();
        // Position with loss exceeding limit (-$1001 loss = -100100 cents)
        let position = create_position_with_state(1, 100, 0, 0, -50000, -50100);

        let result = rm.check_order(&position, Side::Buy, 100, 5000);
        assert_eq!(result, RiskCheckResult::LossTooLarge);
    }

    #[test]
    fn test_check_order_loss_at_limit_allowed() {
        let rm = RiskManager::new();
        // Position with loss exactly at limit (-$1000 loss = -100000 cents)
        let position = create_position_with_state(1, 100, 0, 0, -50000, -50000);

        let result = rm.check_order(&position, Side::Buy, 100, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_profit_always_allowed() {
        let rm = RiskManager::new();
        // Position with profit
        let position = create_position_with_state(1, 100, 0, 0, 50000, 25000);

        let result = rm.check_order(&position, Side::Buy, 100, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    // ==================== Open Orders Check Tests ====================

    #[test]
    fn test_check_open_orders_allowed() {
        let rm = RiskManager::new();

        let result = rm.check_open_orders(1, 50);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_open_orders_at_limit() {
        let rm = RiskManager::new();

        // At limit (100) should reject new order
        let result = rm.check_open_orders(1, 100);
        assert_eq!(result, RiskCheckResult::OpenOrdersTooMany);
    }

    #[test]
    fn test_check_open_orders_over_limit() {
        let rm = RiskManager::new();

        let result = rm.check_open_orders(1, 150);
        assert_eq!(result, RiskCheckResult::OpenOrdersTooMany);
    }

    #[test]
    fn test_check_open_orders_just_under_limit() {
        let rm = RiskManager::new();

        let result = rm.check_open_orders(1, 99);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    // ==================== Position Check Tests ====================

    #[test]
    fn test_check_position_allowed() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 5000, 0, 0, 0, 0);

        let result = rm.check_position(&position);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_position_too_large_long() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 10001, 0, 0, 0, 0);

        let result = rm.check_position(&position);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);
    }

    #[test]
    fn test_check_position_too_large_short() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, -10001, 0, 0, 0, 0);

        let result = rm.check_position(&position);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);
    }

    #[test]
    fn test_check_position_at_limit_allowed() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 10000, 0, 0, 0, 0);

        let result = rm.check_position(&position);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_position_loss_too_large() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 100, 0, 0, -100001, 0);

        let result = rm.check_position(&position);
        assert_eq!(result, RiskCheckResult::LossTooLarge);
    }

    // ==================== Combined Check Tests ====================

    #[test]
    fn test_check_order_with_open_orders_all_pass() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        let result = rm.check_order_with_open_orders(&position, Side::Buy, 100, 5000, 50);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_order_with_open_orders_rejects_open_orders_first() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        // Even though order is valid, too many open orders
        let result = rm.check_order_with_open_orders(&position, Side::Buy, 100, 5000, 100);
        assert_eq!(result, RiskCheckResult::OpenOrdersTooMany);
    }

    #[test]
    fn test_check_order_with_open_orders_checks_order_after_open_orders() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        // Open orders OK, but order too large
        let result = rm.check_order_with_open_orders(&position, Side::Buy, 1001, 5000, 50);
        assert_eq!(result, RiskCheckResult::OrderTooLarge);
    }

    // ==================== Per-Ticker Limits Tests ====================

    #[test]
    fn test_per_ticker_limits() {
        let mut rm = RiskManager::new();

        // Set stricter limits for ticker 1
        rm.set_limits(1, RiskLimits::new(100, 1000, 10000, 10));

        // Ticker 1 should use strict limits
        let position1 = create_position_with_state(1, 0, 0, 0, 0, 0);
        let result = rm.check_order(&position1, Side::Buy, 101, 5000);
        assert_eq!(result, RiskCheckResult::OrderTooLarge);

        // Ticker 2 should use default limits
        let position2 = create_position_with_state(2, 0, 0, 0, 0, 0);
        let result = rm.check_order(&position2, Side::Buy, 101, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_multiple_tickers_different_limits() {
        let mut rm = RiskManager::new();

        rm.set_limits(1, RiskLimits::new(100, 1000, 10000, 10));
        rm.set_limits(2, RiskLimits::new(500, 5000, 50000, 50));

        // Ticker 1: max_order_qty = 100
        let position1 = create_position_with_state(1, 0, 0, 0, 0, 0);
        assert_eq!(
            rm.check_order(&position1, Side::Buy, 100, 5000),
            RiskCheckResult::Allowed
        );
        assert_eq!(
            rm.check_order(&position1, Side::Buy, 101, 5000),
            RiskCheckResult::OrderTooLarge
        );

        // Ticker 2: max_order_qty = 500
        let position2 = create_position_with_state(2, 0, 0, 0, 0, 0);
        assert_eq!(
            rm.check_order(&position2, Side::Buy, 500, 5000),
            RiskCheckResult::Allowed
        );
        assert_eq!(
            rm.check_order(&position2, Side::Buy, 501, 5000),
            RiskCheckResult::OrderTooLarge
        );
    }

    // ==================== Edge Case Tests ====================

    #[test]
    fn test_check_order_zero_qty() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        let result = rm.check_order(&position, Side::Buy, 0, 5000);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_check_position_zero_position() {
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 0, 0, 0, 0, 0);

        let result = rm.check_position(&position);
        assert_eq!(result, RiskCheckResult::Allowed);
    }

    #[test]
    fn test_priority_order_too_large_before_position() {
        // When order is too large, that should be returned even if position would also be too large
        let rm = RiskManager::new();
        let position = create_position_with_state(1, 9999, 0, 0, 0, 0);

        // Order is too large (> 1000) AND would exceed position limit
        let result = rm.check_order(&position, Side::Buy, 2000, 5000);
        assert_eq!(result, RiskCheckResult::OrderTooLarge);
    }

    #[test]
    fn test_priority_position_before_loss() {
        // Position check comes before loss check in check_order
        let mut rm = RiskManager::new();
        rm.set_limits(1, RiskLimits::new(10000, 100, 100000, 100));

        // Position would be too large, and loss is too large
        let position = create_position_with_state(1, 99, 0, 0, -200000, 0);

        let result = rm.check_order(&position, Side::Buy, 100, 5000);
        assert_eq!(result, RiskCheckResult::PositionTooLarge);
    }

    #[test]
    fn test_default_impl() {
        let rm = RiskManager::default();
        assert_eq!(rm.default_limits.max_order_qty, 1000);
    }
}
