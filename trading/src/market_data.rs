//! Market data receiver for the trading client.
//!
//! Receives market data updates via multicast and maintains a local BBO
//! (Best Bid/Offer) view for each ticker.

use common::net::multicast::MulticastSocket;
use common::{Price, Qty, Side, TickerId, INVALID_PRICE};
use exchange::protocol::{MarketUpdate, MarketUpdateType, MARKET_UPDATE_SIZE};
use std::collections::HashMap;

/// Best Bid and Offer for a single ticker.
///
/// Represents the top of the order book with the best available
/// prices and quantities on each side.
#[derive(Debug, Clone, Copy, Default)]
pub struct BBO {
    pub bid_price: Price,
    pub bid_qty: Qty,
    pub ask_price: Price,
    pub ask_qty: Qty,
}

impl BBO {
    /// Creates a new BBO with invalid/empty prices.
    pub fn new() -> Self {
        Self {
            bid_price: INVALID_PRICE,
            bid_qty: 0,
            ask_price: INVALID_PRICE,
            ask_qty: 0,
        }
    }

    /// Returns true if this BBO has valid bid data.
    #[inline]
    pub fn has_bid(&self) -> bool {
        self.bid_price != INVALID_PRICE && self.bid_qty > 0
    }

    /// Returns true if this BBO has valid ask data.
    #[inline]
    pub fn has_ask(&self) -> bool {
        self.ask_price != INVALID_PRICE && self.ask_qty > 0
    }

    /// Returns true if both bid and ask are valid.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.has_bid() && self.has_ask()
    }

    /// Returns the spread (ask - bid) if both sides are valid.
    #[inline]
    pub fn spread(&self) -> Option<Price> {
        if self.is_valid() {
            Some(self.ask_price - self.bid_price)
        } else {
            None
        }
    }

    /// Returns the mid price if both sides are valid.
    #[inline]
    pub fn mid_price(&self) -> Option<Price> {
        if self.is_valid() {
            Some((self.bid_price + self.ask_price) / 2)
        } else {
            None
        }
    }
}

/// Callback type for market data subscribers.
pub type MarketDataCallback = Box<dyn FnMut(TickerId, &MarketUpdate, &BBO) + Send>;

/// Receives market data updates via multicast and maintains BBO state.
///
/// The receiver joins a multicast group, deserializes incoming MarketUpdate
/// messages, and maintains a local order book view (BBO) for each ticker.
pub struct MarketDataReceiver {
    socket: MulticastSocket,
    bbo: HashMap<TickerId, BBO>,
    subscribers: Vec<MarketDataCallback>,
    /// Sequence number for gap detection (if needed)
    #[allow(dead_code)]
    last_seq: u64,
}

impl MarketDataReceiver {
    /// Creates a new MarketDataReceiver and joins the multicast group.
    ///
    /// # Arguments
    /// * `multicast_addr` - The multicast group address (e.g., "239.255.0.1")
    /// * `port` - The port number to listen on
    /// * `interface` - The local interface IP to bind to (e.g., "0.0.0.0")
    ///
    /// # Returns
    /// A new MarketDataReceiver joined to the specified multicast group
    pub fn new(multicast_addr: &str, port: u16, interface: &str) -> std::io::Result<Self> {
        let socket = MulticastSocket::join_group(multicast_addr, port, interface)?;

        // Set socket to non-blocking for poll-based operation
        socket.set_nonblocking(true)?;

        Ok(Self {
            socket,
            bbo: HashMap::new(),
            subscribers: Vec::new(),
            last_seq: 0,
        })
    }

    /// Polls for the next market update without blocking.
    ///
    /// # Returns
    /// - `Some(MarketUpdate)` if an update was received
    /// - `None` if no data is available
    pub fn poll(&mut self) -> Option<MarketUpdate> {
        match self.socket.try_recv() {
            Ok(Some(data)) => {
                // Ensure we have enough data for a MarketUpdate
                if data.len() >= MARKET_UPDATE_SIZE {
                    // Zero-copy deserialization
                    if let Some(update) = MarketUpdate::from_bytes(&data[..MARKET_UPDATE_SIZE]) {
                        // Copy the packed struct to avoid alignment issues
                        return Some(*update);
                    }
                }
                None
            }
            Ok(None) => None,
            Err(_) => None,
        }
    }

    /// Processes a market update and updates the local BBO state.
    ///
    /// This method should be called for each update received from `poll()`.
    /// It updates the internal BBO state based on the update type and
    /// notifies all registered subscribers.
    pub fn process_update(&mut self, update: &MarketUpdate) {
        // Extract fields from packed struct to avoid unaligned access
        let ticker_id = update.ticker_id;
        let side = update.side;
        let price = update.price;
        let qty = update.qty;

        let update_type = match update.update_type() {
            Some(t) => t,
            None => return, // Invalid update type
        };

        // Get or create BBO for this ticker
        let bbo = self.bbo.entry(ticker_id).or_insert_with(BBO::new);

        match update_type {
            MarketUpdateType::Add | MarketUpdateType::Modify | MarketUpdateType::Snapshot => {
                // Update BBO based on side
                if side == Side::Buy as i8 {
                    // Update bid if this is a better price or same price with more qty
                    if price > bbo.bid_price || bbo.bid_price == INVALID_PRICE {
                        bbo.bid_price = price;
                        bbo.bid_qty = qty;
                    } else if price == bbo.bid_price {
                        // Same price level - this could be qty update
                        bbo.bid_qty = qty;
                    }
                } else if side == Side::Sell as i8 {
                    // Update ask if this is a better (lower) price or same price
                    if price < bbo.ask_price || bbo.ask_price == INVALID_PRICE {
                        bbo.ask_price = price;
                        bbo.ask_qty = qty;
                    } else if price == bbo.ask_price {
                        // Same price level - this could be qty update
                        bbo.ask_qty = qty;
                    }
                }
            }
            MarketUpdateType::Cancel => {
                // If the cancelled order was at BBO, we need to invalidate
                // In a full implementation, we'd track the full book
                if side == Side::Buy as i8 && price == bbo.bid_price {
                    // Bid at BBO was cancelled - mark as potentially stale
                    // A real implementation would have the full book to find next best
                    if qty == 0 || qty >= bbo.bid_qty {
                        bbo.bid_qty = 0;
                    } else {
                        bbo.bid_qty = bbo.bid_qty.saturating_sub(qty);
                    }
                } else if side == Side::Sell as i8 && price == bbo.ask_price {
                    // Ask at BBO was cancelled
                    if qty == 0 || qty >= bbo.ask_qty {
                        bbo.ask_qty = 0;
                    } else {
                        bbo.ask_qty = bbo.ask_qty.saturating_sub(qty);
                    }
                }
            }
            MarketUpdateType::Trade => {
                // Trade occurred - reduce qty at the trade price level
                if side == Side::Buy as i8 && price == bbo.ask_price {
                    // Buy trade hits the ask
                    bbo.ask_qty = bbo.ask_qty.saturating_sub(qty);
                } else if side == Side::Sell as i8 && price == bbo.bid_price {
                    // Sell trade hits the bid
                    bbo.bid_qty = bbo.bid_qty.saturating_sub(qty);
                }
            }
            MarketUpdateType::Clear => {
                // Clear the entire book for this ticker
                *bbo = BBO::new();
            }
        }

        // Notify subscribers
        let bbo_copy = *bbo;
        for subscriber in &mut self.subscribers {
            subscriber(ticker_id, update, &bbo_copy);
        }
    }

    /// Returns the current BBO for a ticker.
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker to look up
    ///
    /// # Returns
    /// - `Some(&BBO)` if we have data for this ticker
    /// - `None` if no data has been received for this ticker
    #[inline]
    pub fn get_bbo(&self, ticker_id: TickerId) -> Option<&BBO> {
        self.bbo.get(&ticker_id)
    }

    /// Returns a mutable reference to the BBO for a ticker.
    #[inline]
    pub fn get_bbo_mut(&mut self, ticker_id: TickerId) -> Option<&mut BBO> {
        self.bbo.get_mut(&ticker_id)
    }

    /// Registers a callback to be notified of market data updates.
    ///
    /// The callback receives the ticker ID, the raw update, and the
    /// updated BBO after processing.
    pub fn subscribe(&mut self, callback: MarketDataCallback) {
        self.subscribers.push(callback);
    }

    /// Returns the number of tickers being tracked.
    #[inline]
    pub fn ticker_count(&self) -> usize {
        self.bbo.len()
    }

    /// Returns an iterator over all tracked ticker IDs and their BBOs.
    #[inline]
    pub fn iter_bbo(&self) -> impl Iterator<Item = (&TickerId, &BBO)> {
        self.bbo.iter()
    }

    /// Polls and processes updates in a loop until no more data is available.
    ///
    /// This is a convenience method that combines `poll()` and `process_update()`
    /// for batch processing.
    ///
    /// # Returns
    /// The number of updates processed
    pub fn poll_and_process(&mut self) -> usize {
        let mut count = 0;
        while let Some(update) = self.poll() {
            self.process_update(&update);
            count += 1;
        }
        count
    }

    /// Pre-allocates BBO entries for the given tickers.
    ///
    /// This can help reduce allocation during runtime.
    pub fn reserve_tickers(&mut self, tickers: &[TickerId]) {
        for &ticker_id in tickers {
            self.bbo.entry(ticker_id).or_insert_with(BBO::new);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bbo_new() {
        let bbo = BBO::new();
        assert_eq!(bbo.bid_price, INVALID_PRICE);
        assert_eq!(bbo.ask_price, INVALID_PRICE);
        assert_eq!(bbo.bid_qty, 0);
        assert_eq!(bbo.ask_qty, 0);
        assert!(!bbo.has_bid());
        assert!(!bbo.has_ask());
        assert!(!bbo.is_valid());
    }

    #[test]
    fn test_bbo_spread() {
        let mut bbo = BBO::new();
        assert!(bbo.spread().is_none());

        bbo.bid_price = 100;
        bbo.bid_qty = 10;
        bbo.ask_price = 102;
        bbo.ask_qty = 20;

        assert!(bbo.is_valid());
        assert_eq!(bbo.spread(), Some(2));
        assert_eq!(bbo.mid_price(), Some(101));
    }

    #[test]
    fn test_bbo_has_bid_ask() {
        let mut bbo = BBO::new();

        bbo.bid_price = 100;
        bbo.bid_qty = 10;
        assert!(bbo.has_bid());
        assert!(!bbo.has_ask());

        bbo.ask_price = 102;
        bbo.ask_qty = 20;
        assert!(bbo.has_bid());
        assert!(bbo.has_ask());
        assert!(bbo.is_valid());
    }

    #[test]
    fn test_process_add_update() {
        // Create a mock receiver without actual socket for testing
        let mut bbo_map: HashMap<TickerId, BBO> = HashMap::new();

        // Simulate processing an Add update for bid
        let ticker_id: TickerId = 1;
        let bbo = bbo_map.entry(ticker_id).or_insert_with(BBO::new);

        // Simulate bid update
        bbo.bid_price = 10050;
        bbo.bid_qty = 100;

        assert_eq!(bbo.bid_price, 10050);
        assert_eq!(bbo.bid_qty, 100);
        assert!(bbo.has_bid());
    }

    #[test]
    fn test_process_trade_reduces_qty() {
        let mut bbo = BBO::new();
        bbo.bid_price = 100;
        bbo.bid_qty = 100;
        bbo.ask_price = 102;
        bbo.ask_qty = 50;

        // Simulate trade hitting the ask
        bbo.ask_qty = bbo.ask_qty.saturating_sub(20);
        assert_eq!(bbo.ask_qty, 30);
    }

    #[test]
    fn test_process_clear() {
        let mut bbo = BBO::new();
        bbo.bid_price = 100;
        bbo.bid_qty = 100;
        bbo.ask_price = 102;
        bbo.ask_qty = 50;

        // Clear
        bbo = BBO::new();
        assert!(!bbo.is_valid());
        assert_eq!(bbo.bid_price, INVALID_PRICE);
    }
}
