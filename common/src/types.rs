// Core type definitions for the trading system

// Type aliases
pub type OrderId = u64;
pub type TickerId = u32;
pub type ClientId = u32;
pub type Price = i64;      // Fixed-point (cents)
pub type Qty = u32;
pub type Priority = u64;   // Timestamp-based queue priority

// Invalid/sentinel constants
pub const INVALID_ORDER_ID: OrderId = 0;
pub const INVALID_TICKER_ID: TickerId = u32::MAX;
pub const INVALID_CLIENT_ID: ClientId = u32::MAX;
pub const INVALID_PRICE: Price = i64::MAX;
pub const INVALID_QTY: Qty = u32::MAX;

/// Represents the side of an order (buy or sell)
#[repr(i8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy = 1,
    Sell = -1,
}

impl Side {
    /// Returns the opposite side
    #[inline]
    pub fn opposite(&self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// Returns the side as a sign value (1 for Buy, -1 for Sell)
    #[inline]
    pub fn as_sign(&self) -> i64 {
        *self as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_side_opposite() {
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Side::Sell.opposite(), Side::Buy);
    }

    #[test]
    fn test_side_as_sign() {
        assert_eq!(Side::Buy.as_sign(), 1);
        assert_eq!(Side::Sell.as_sign(), -1);
    }

    #[test]
    fn test_invalid_constants() {
        assert_eq!(INVALID_ORDER_ID, 0);
        assert_eq!(INVALID_TICKER_ID, u32::MAX);
        assert_eq!(INVALID_CLIENT_ID, u32::MAX);
        assert_eq!(INVALID_PRICE, i64::MAX);
        assert_eq!(INVALID_QTY, u32::MAX);
    }
}
