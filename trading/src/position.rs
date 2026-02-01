// Position tracking

use common::{Price, Qty, Side, TickerId};
use std::collections::HashMap;

/// Tracks position and P&L for a single ticker
#[derive(Debug, Clone, Default)]
pub struct Position {
    /// Ticker identifier
    pub ticker_id: TickerId,
    /// Current position (positive = long, negative = short)
    pub position: i64,
    /// Pending buy order quantity
    pub open_buy_qty: Qty,
    /// Pending sell order quantity
    pub open_sell_qty: Qty,
    /// Total traded volume
    pub volume_traded: u64,
    /// Realized P&L in cents
    pub realized_pnl: i64,
    /// Unrealized P&L in cents
    pub unrealized_pnl: i64,
    /// Average entry price for open position (for P&L calculation)
    pub avg_open_price: Price,
    /// Last traded/quoted price
    pub last_price: Price,
}

impl Position {
    /// Creates a new position tracker for the given ticker
    pub fn new(ticker_id: TickerId) -> Self {
        Self {
            ticker_id,
            position: 0,
            open_buy_qty: 0,
            open_sell_qty: 0,
            volume_traded: 0,
            realized_pnl: 0,
            unrealized_pnl: 0,
            avg_open_price: 0,
            last_price: 0,
        }
    }

    /// Update position on fill
    ///
    /// Handles the P&L and average price calculations when a trade fills.
    /// When closing or reducing a position, realized P&L is calculated.
    /// When opening or adding to a position, average price is updated.
    pub fn on_fill(&mut self, side: Side, qty: Qty, price: Price) {
        let signed_qty = match side {
            Side::Buy => qty as i64,
            Side::Sell => -(qty as i64),
        };

        // Update volume traded
        self.volume_traded += qty as u64;

        // Update last price
        self.last_price = price;

        let old_position = self.position;
        let new_position = old_position + signed_qty;

        // Determine if we're closing, opening, or both
        if old_position == 0 {
            // Opening new position
            self.avg_open_price = price;
        } else if (old_position > 0 && signed_qty < 0) || (old_position < 0 && signed_qty > 0) {
            // Closing or reducing position (or flipping)
            let closing_qty = old_position.abs().min(signed_qty.abs());

            // Calculate realized P&L on the closing portion
            // P&L = (exit_price - entry_price) * qty * direction
            let pnl_per_unit = if old_position > 0 {
                // Was long, selling to close
                price - self.avg_open_price
            } else {
                // Was short, buying to close
                self.avg_open_price - price
            };
            self.realized_pnl += pnl_per_unit * closing_qty;

            // Check if we're flipping the position
            if new_position != 0 && (new_position > 0) != (old_position > 0) {
                // Flipping position - new portion at new price
                self.avg_open_price = price;
            }
            // If fully closed or reduced, avg_open_price stays the same for remaining position
        } else {
            // Adding to existing position - update weighted average price
            let total_cost = self.avg_open_price * old_position.abs() + price * signed_qty.abs();
            self.avg_open_price = total_cost / new_position.abs();
        }

        self.position = new_position;

        // Update unrealized P&L
        self.update_unrealized_pnl();
    }

    /// Add pending order quantity
    pub fn add_open_order(&mut self, side: Side, qty: Qty) {
        match side {
            Side::Buy => self.open_buy_qty += qty,
            Side::Sell => self.open_sell_qty += qty,
        }
    }

    /// Remove pending order quantity (on cancel or fill)
    pub fn remove_open_order(&mut self, side: Side, qty: Qty) {
        match side {
            Side::Buy => self.open_buy_qty = self.open_buy_qty.saturating_sub(qty),
            Side::Sell => self.open_sell_qty = self.open_sell_qty.saturating_sub(qty),
        }
    }

    /// Update market price (for unrealized P&L calculation)
    pub fn update_market_price(&mut self, price: Price) {
        self.last_price = price;
        self.update_unrealized_pnl();
    }

    /// Returns the current net position
    #[inline]
    pub fn net_position(&self) -> i64 {
        self.position
    }

    /// Returns total P&L (realized + unrealized)
    #[inline]
    pub fn total_pnl(&self) -> i64 {
        self.realized_pnl + self.unrealized_pnl
    }

    /// Returns maximum long exposure (position + pending buys)
    #[inline]
    pub fn max_long_exposure(&self) -> i64 {
        self.position + self.open_buy_qty as i64
    }

    /// Returns maximum short exposure (position - pending sells)
    #[inline]
    pub fn max_short_exposure(&self) -> i64 {
        self.position - self.open_sell_qty as i64
    }

    /// Update unrealized P&L based on current position and last price
    fn update_unrealized_pnl(&mut self) {
        if self.position == 0 {
            self.unrealized_pnl = 0;
        } else if self.position > 0 {
            // Long position: profit if price goes up
            self.unrealized_pnl = (self.last_price - self.avg_open_price) * self.position;
        } else {
            // Short position: profit if price goes down
            self.unrealized_pnl = (self.avg_open_price - self.last_price) * (-self.position);
        }
    }
}

/// Manages positions across all tickers
pub struct PositionKeeper {
    /// Per-ticker position tracking
    positions: HashMap<TickerId, Position>,
    /// Cached total P&L across all positions
    total_pnl: i64,
}

impl PositionKeeper {
    /// Creates a new position keeper
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            total_pnl: 0,
        }
    }

    /// Get read-only reference to a position
    pub fn get_position(&self, ticker_id: TickerId) -> Option<&Position> {
        self.positions.get(&ticker_id)
    }

    /// Get mutable reference to a position, creating it if necessary
    pub fn get_position_mut(&mut self, ticker_id: TickerId) -> &mut Position {
        self.positions
            .entry(ticker_id)
            .or_insert_with(|| Position::new(ticker_id))
    }

    /// Process a fill for a ticker
    pub fn on_fill(&mut self, ticker_id: TickerId, side: Side, qty: Qty, price: Price) {
        let position = self.get_position_mut(ticker_id);
        position.on_fill(side, qty, price);
        self.recalculate_total_pnl();
    }

    /// Update market price for a ticker
    pub fn update_market_price(&mut self, ticker_id: TickerId, price: Price) {
        if let Some(position) = self.positions.get_mut(&ticker_id) {
            position.update_market_price(price);
            self.recalculate_total_pnl();
        }
    }

    /// Get total P&L across all positions
    #[inline]
    pub fn total_pnl(&self) -> i64 {
        self.total_pnl
    }

    /// Iterate over all positions
    pub fn all_positions(&self) -> impl Iterator<Item = &Position> {
        self.positions.values()
    }

    /// Recalculate total P&L from all positions
    fn recalculate_total_pnl(&mut self) {
        self.total_pnl = self.positions.values().map(|p| p.total_pnl()).sum();
    }
}

impl Default for PositionKeeper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_new() {
        let pos = Position::new(1);
        assert_eq!(pos.ticker_id, 1);
        assert_eq!(pos.position, 0);
        assert_eq!(pos.open_buy_qty, 0);
        assert_eq!(pos.open_sell_qty, 0);
        assert_eq!(pos.volume_traded, 0);
        assert_eq!(pos.realized_pnl, 0);
        assert_eq!(pos.unrealized_pnl, 0);
    }

    #[test]
    fn test_buy_fill_opens_long() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000); // Buy 100 @ $50.00

        assert_eq!(pos.position, 100);
        assert_eq!(pos.avg_open_price, 5000);
        assert_eq!(pos.volume_traded, 100);
        assert_eq!(pos.realized_pnl, 0);
    }

    #[test]
    fn test_sell_fill_opens_short() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Sell, 100, 5000); // Sell 100 @ $50.00

        assert_eq!(pos.position, -100);
        assert_eq!(pos.avg_open_price, 5000);
        assert_eq!(pos.volume_traded, 100);
        assert_eq!(pos.realized_pnl, 0);
    }

    #[test]
    fn test_partial_close_long_with_profit() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000); // Buy 100 @ $50.00
        pos.on_fill(Side::Sell, 50, 5500); // Sell 50 @ $55.00

        assert_eq!(pos.position, 50);
        assert_eq!(pos.avg_open_price, 5000); // Average stays same
        assert_eq!(pos.volume_traded, 150);
        // Profit = (55.00 - 50.00) * 50 = $250 = 25000 cents
        assert_eq!(pos.realized_pnl, 25000);
    }

    #[test]
    fn test_partial_close_long_with_loss() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000); // Buy 100 @ $50.00
        pos.on_fill(Side::Sell, 50, 4500); // Sell 50 @ $45.00

        assert_eq!(pos.position, 50);
        // Loss = (45.00 - 50.00) * 50 = -$250 = -25000 cents
        assert_eq!(pos.realized_pnl, -25000);
    }

    #[test]
    fn test_partial_close_short_with_profit() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Sell, 100, 5000); // Sell short 100 @ $50.00
        pos.on_fill(Side::Buy, 50, 4500); // Buy to cover 50 @ $45.00

        assert_eq!(pos.position, -50);
        // Profit = (50.00 - 45.00) * 50 = $250 = 25000 cents
        assert_eq!(pos.realized_pnl, 25000);
    }

    #[test]
    fn test_partial_close_short_with_loss() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Sell, 100, 5000); // Sell short 100 @ $50.00
        pos.on_fill(Side::Buy, 50, 5500); // Buy to cover 50 @ $55.00

        assert_eq!(pos.position, -50);
        // Loss = (50.00 - 55.00) * 50 = -$250 = -25000 cents
        assert_eq!(pos.realized_pnl, -25000);
    }

    #[test]
    fn test_full_close_long() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000);
        pos.on_fill(Side::Sell, 100, 5200);

        assert_eq!(pos.position, 0);
        assert_eq!(pos.realized_pnl, 20000); // $200 profit
        assert_eq!(pos.unrealized_pnl, 0);
    }

    #[test]
    fn test_add_to_long_position() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000); // Buy 100 @ $50.00
        pos.on_fill(Side::Buy, 100, 6000); // Buy 100 @ $60.00

        assert_eq!(pos.position, 200);
        // Weighted average: (100*5000 + 100*6000) / 200 = 5500
        assert_eq!(pos.avg_open_price, 5500);
        assert_eq!(pos.volume_traded, 200);
        assert_eq!(pos.realized_pnl, 0);
    }

    #[test]
    fn test_add_to_short_position() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Sell, 100, 5000); // Sell 100 @ $50.00
        pos.on_fill(Side::Sell, 100, 4000); // Sell 100 @ $40.00

        assert_eq!(pos.position, -200);
        // Weighted average: (100*5000 + 100*4000) / 200 = 4500
        assert_eq!(pos.avg_open_price, 4500);
    }

    #[test]
    fn test_unrealized_pnl_long() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000); // Buy 100 @ $50.00

        pos.update_market_price(5500); // Price rises to $55.00
        assert_eq!(pos.unrealized_pnl, 50000); // $500 unrealized profit

        pos.update_market_price(4500); // Price drops to $45.00
        assert_eq!(pos.unrealized_pnl, -50000); // $500 unrealized loss
    }

    #[test]
    fn test_unrealized_pnl_short() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Sell, 100, 5000); // Sell 100 @ $50.00

        pos.update_market_price(4500); // Price drops to $45.00
        assert_eq!(pos.unrealized_pnl, 50000); // $500 unrealized profit

        pos.update_market_price(5500); // Price rises to $55.00
        assert_eq!(pos.unrealized_pnl, -50000); // $500 unrealized loss
    }

    #[test]
    fn test_total_pnl() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000); // Buy 100 @ $50.00
        pos.on_fill(Side::Sell, 50, 5500); // Sell 50 @ $55.00 (realize $250)
        pos.update_market_price(5200); // Remaining 50 @ $52.00

        // Realized: 25000 cents ($250)
        // Unrealized: (5200 - 5000) * 50 = 10000 cents ($100)
        assert_eq!(pos.realized_pnl, 25000);
        assert_eq!(pos.unrealized_pnl, 10000);
        assert_eq!(pos.total_pnl(), 35000);
    }

    #[test]
    fn test_open_order_tracking() {
        let mut pos = Position::new(1);
        pos.position = 100;

        pos.add_open_order(Side::Buy, 50);
        pos.add_open_order(Side::Sell, 30);

        assert_eq!(pos.open_buy_qty, 50);
        assert_eq!(pos.open_sell_qty, 30);
        assert_eq!(pos.max_long_exposure(), 150); // 100 + 50
        assert_eq!(pos.max_short_exposure(), 70); // 100 - 30

        pos.remove_open_order(Side::Buy, 20);
        pos.remove_open_order(Side::Sell, 10);

        assert_eq!(pos.open_buy_qty, 30);
        assert_eq!(pos.open_sell_qty, 20);
    }

    #[test]
    fn test_open_order_saturating_sub() {
        let mut pos = Position::new(1);
        pos.add_open_order(Side::Buy, 10);
        pos.remove_open_order(Side::Buy, 100); // Remove more than exists

        assert_eq!(pos.open_buy_qty, 0); // Should not underflow
    }

    #[test]
    fn test_net_position() {
        let mut pos = Position::new(1);
        assert_eq!(pos.net_position(), 0);

        pos.on_fill(Side::Buy, 100, 5000);
        assert_eq!(pos.net_position(), 100);

        pos.on_fill(Side::Sell, 150, 5000);
        assert_eq!(pos.net_position(), -50);
    }

    #[test]
    fn test_position_keeper_new() {
        let keeper = PositionKeeper::new();
        assert_eq!(keeper.total_pnl(), 0);
        assert_eq!(keeper.all_positions().count(), 0);
    }

    #[test]
    fn test_position_keeper_get_position() {
        let mut keeper = PositionKeeper::new();

        // Get non-existent position
        assert!(keeper.get_position(1).is_none());

        // Get or create position
        let pos = keeper.get_position_mut(1);
        pos.on_fill(Side::Buy, 100, 5000);

        // Now it should exist
        let pos = keeper.get_position(1).unwrap();
        assert_eq!(pos.position, 100);
    }

    #[test]
    fn test_position_keeper_on_fill() {
        let mut keeper = PositionKeeper::new();

        keeper.on_fill(1, Side::Buy, 100, 5000);
        keeper.on_fill(2, Side::Sell, 50, 3000);

        let pos1 = keeper.get_position(1).unwrap();
        assert_eq!(pos1.position, 100);

        let pos2 = keeper.get_position(2).unwrap();
        assert_eq!(pos2.position, -50);
    }

    #[test]
    fn test_position_keeper_total_pnl() {
        let mut keeper = PositionKeeper::new();

        // Ticker 1: Buy, then price goes up
        keeper.on_fill(1, Side::Buy, 100, 5000);
        keeper.update_market_price(1, 5500);

        // Ticker 2: Sell short, then price goes down
        keeper.on_fill(2, Side::Sell, 100, 4000);
        keeper.update_market_price(2, 3500);

        // Unrealized P&L ticker 1: (5500 - 5000) * 100 = 50000
        // Unrealized P&L ticker 2: (4000 - 3500) * 100 = 50000
        assert_eq!(keeper.total_pnl(), 100000);
    }

    #[test]
    fn test_position_keeper_all_positions() {
        let mut keeper = PositionKeeper::new();

        keeper.on_fill(1, Side::Buy, 100, 5000);
        keeper.on_fill(2, Side::Sell, 50, 3000);
        keeper.on_fill(3, Side::Buy, 200, 4000);

        let positions: Vec<_> = keeper.all_positions().collect();
        assert_eq!(positions.len(), 3);
    }

    #[test]
    fn test_position_flip_long_to_short() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000); // Long 100 @ $50
        pos.on_fill(Side::Sell, 150, 5500); // Sell 150 @ $55 (close 100, open short 50)

        assert_eq!(pos.position, -50);
        // Realized from closing long: (5500 - 5000) * 100 = 50000
        assert_eq!(pos.realized_pnl, 50000);
        // New short position at $55
        assert_eq!(pos.avg_open_price, 5500);
    }

    #[test]
    fn test_position_flip_short_to_long() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Sell, 100, 5000); // Short 100 @ $50
        pos.on_fill(Side::Buy, 150, 4500); // Buy 150 @ $45 (cover 100, open long 50)

        assert_eq!(pos.position, 50);
        // Realized from covering short: (5000 - 4500) * 100 = 50000
        assert_eq!(pos.realized_pnl, 50000);
        // New long position at $45
        assert_eq!(pos.avg_open_price, 4500);
    }

    #[test]
    fn test_volume_accumulation() {
        let mut pos = Position::new(1);
        pos.on_fill(Side::Buy, 100, 5000);
        pos.on_fill(Side::Buy, 50, 5100);
        pos.on_fill(Side::Sell, 75, 5200);

        assert_eq!(pos.volume_traded, 225);
    }
}
