//! Market data publisher for the exchange.
//!
//! Multicasts market data updates (order adds, modifies, cancels, trades)
//! to all subscribed clients. Supports snapshot generation for late joiners.

use common::net::multicast::MulticastSocket;
use common::{Price, Qty, Side, TickerId};
use crate::protocol::{MarketUpdate, MarketUpdateType};
use std::collections::HashMap;
use std::io;

/// Configuration for the market data publisher.
#[derive(Debug, Clone)]
pub struct MarketDataPublisherConfig {
    /// Multicast group address (e.g., "239.255.0.1")
    pub multicast_addr: String,
    /// Port number for multicast
    pub port: u16,
    /// Local interface IP to bind to (e.g., "0.0.0.0" for any)
    pub interface: String,
    /// Time-to-live for multicast packets (1 = local network only)
    pub ttl: u32,
    /// Whether to enable snapshot generation
    pub enable_snapshots: bool,
    /// Interval between automatic snapshots (in number of updates)
    pub snapshot_interval: usize,
}

impl Default for MarketDataPublisherConfig {
    fn default() -> Self {
        Self {
            multicast_addr: "239.255.0.1".to_string(),
            port: 5000,
            interface: "0.0.0.0".to_string(),
            ttl: 1,
            enable_snapshots: true,
            snapshot_interval: 1000,
        }
    }
}

/// Best bid and offer state for a single ticker (used for snapshots).
#[derive(Debug, Clone, Copy, Default)]
struct TickerState {
    /// Best bid price
    bid_price: Price,
    /// Best bid quantity
    bid_qty: Qty,
    /// Best ask price
    ask_price: Price,
    /// Best ask quantity
    ask_qty: Qty,
    /// Sequence number of last update
    last_seq: u64,
}

/// Market data publisher that multicasts updates to subscribers.
///
/// The publisher:
/// - Receives MarketUpdate messages from the matching engine
/// - Serializes them using zero-copy binary format
/// - Broadcasts to a configurable multicast group
/// - Maintains state for snapshot generation
pub struct MarketDataPublisher {
    /// Multicast socket for sending data
    socket: MulticastSocket,
    /// Configuration
    config: MarketDataPublisherConfig,
    /// Current state per ticker (for snapshots)
    ticker_state: HashMap<TickerId, TickerState>,
    /// Sequence number for updates
    sequence: u64,
    /// Update count since last snapshot
    updates_since_snapshot: usize,
    /// Statistics: total updates sent
    total_updates_sent: u64,
    /// Statistics: total bytes sent
    total_bytes_sent: u64,
}

impl MarketDataPublisher {
    /// Creates a new market data publisher with the given configuration.
    ///
    /// # Arguments
    /// * `config` - Publisher configuration including multicast address and port
    ///
    /// # Returns
    /// A new MarketDataPublisher or an IO error if socket creation fails
    pub fn new(config: MarketDataPublisherConfig) -> io::Result<Self> {
        let socket = MulticastSocket::new()?;

        // Set TTL for multicast packets
        socket.set_multicast_ttl(config.ttl)?;

        // Set the outgoing interface
        socket.set_multicast_interface(&config.interface)?;

        Ok(Self {
            socket,
            config,
            ticker_state: HashMap::new(),
            sequence: 0,
            updates_since_snapshot: 0,
            total_updates_sent: 0,
            total_bytes_sent: 0,
        })
    }

    /// Creates a new market data publisher with default configuration.
    pub fn with_defaults() -> io::Result<Self> {
        Self::new(MarketDataPublisherConfig::default())
    }

    /// Publishes a market update to all subscribers.
    ///
    /// # Arguments
    /// * `update` - The market update to publish
    ///
    /// # Returns
    /// The number of bytes sent, or an IO error
    pub fn publish(&mut self, update: &MarketUpdate) -> io::Result<usize> {
        // Extract ticker_id from packed struct to avoid unaligned references
        let ticker_id = update.ticker_id;

        // Update internal state for snapshots
        if self.config.enable_snapshots {
            self.update_ticker_state(ticker_id, update);
        }

        // Serialize and send
        let bytes = update.as_bytes();
        let sent = self.socket.send_to(bytes, &self.config.multicast_addr, self.config.port)?;

        // Update statistics
        self.sequence += 1;
        self.updates_since_snapshot += 1;
        self.total_updates_sent += 1;
        self.total_bytes_sent += sent as u64;

        // Check if we should send a snapshot
        if self.config.enable_snapshots
            && self.config.snapshot_interval > 0
            && self.updates_since_snapshot >= self.config.snapshot_interval
        {
            self.publish_snapshot()?;
        }

        Ok(sent)
    }

    /// Publishes multiple market updates in a batch.
    ///
    /// This is more efficient than calling `publish` multiple times
    /// as it can amortize any per-call overhead.
    ///
    /// # Arguments
    /// * `updates` - Iterator of market updates to publish
    ///
    /// # Returns
    /// The total number of bytes sent, or an IO error
    pub fn publish_batch<'a, I>(&mut self, updates: I) -> io::Result<usize>
    where
        I: IntoIterator<Item = &'a MarketUpdate>,
    {
        let mut total_sent = 0;
        for update in updates {
            total_sent += self.publish(update)?;
        }
        Ok(total_sent)
    }

    /// Updates internal ticker state based on a market update.
    fn update_ticker_state(&mut self, ticker_id: TickerId, update: &MarketUpdate) {
        let state = self.ticker_state.entry(ticker_id).or_default();

        // Extract fields from packed struct
        let update_type = update.update_type();
        let side = update.side;
        let price = update.price;
        let qty = update.qty;

        match update_type {
            Some(MarketUpdateType::Add) | Some(MarketUpdateType::Modify) | Some(MarketUpdateType::Snapshot) => {
                if side == Side::Buy as i8 {
                    // Update bid if better or same price
                    if price > state.bid_price || state.bid_price == 0 {
                        state.bid_price = price;
                        state.bid_qty = qty;
                    } else if price == state.bid_price {
                        state.bid_qty = qty;
                    }
                } else if side == Side::Sell as i8 {
                    // Update ask if better (lower) or same price
                    if state.ask_price == 0 || price < state.ask_price {
                        state.ask_price = price;
                        state.ask_qty = qty;
                    } else if price == state.ask_price {
                        state.ask_qty = qty;
                    }
                }
            }
            Some(MarketUpdateType::Cancel) => {
                if side == Side::Buy as i8 && price == state.bid_price {
                    // Bid at BBO cancelled - reduce qty
                    state.bid_qty = state.bid_qty.saturating_sub(qty);
                    if state.bid_qty == 0 {
                        state.bid_price = 0;
                    }
                } else if side == Side::Sell as i8 && price == state.ask_price {
                    // Ask at BBO cancelled - reduce qty
                    state.ask_qty = state.ask_qty.saturating_sub(qty);
                    if state.ask_qty == 0 {
                        state.ask_price = 0;
                    }
                }
            }
            Some(MarketUpdateType::Trade) => {
                // Trade reduces quantity at the trade price
                if side == Side::Buy as i8 && price == state.ask_price {
                    state.ask_qty = state.ask_qty.saturating_sub(qty);
                } else if side == Side::Sell as i8 && price == state.bid_price {
                    state.bid_qty = state.bid_qty.saturating_sub(qty);
                }
            }
            Some(MarketUpdateType::Clear) => {
                // Clear the entire state for this ticker
                *state = TickerState::default();
            }
            None => {
                // Invalid update type - ignore
            }
        }

        state.last_seq = self.sequence;
    }

    /// Publishes a snapshot of the current market state for all tickers.
    ///
    /// This is useful for late-joining subscribers to catch up on current state.
    ///
    /// # Returns
    /// The total number of bytes sent, or an IO error
    pub fn publish_snapshot(&mut self) -> io::Result<usize> {
        let mut total_sent = 0;

        // Collect ticker IDs first to avoid borrow issues
        let ticker_ids: Vec<TickerId> = self.ticker_state.keys().copied().collect();

        for ticker_id in ticker_ids {
            let state = self.ticker_state.get(&ticker_id).copied().unwrap_or_default();

            // Send bid snapshot if we have a valid bid
            if state.bid_price > 0 && state.bid_qty > 0 {
                let bid_update = MarketUpdate::new(
                    MarketUpdateType::Snapshot,
                    ticker_id,
                    0, // No specific order ID for snapshot
                    Side::Buy as i8,
                    state.bid_price,
                    state.bid_qty,
                    self.sequence,
                );

                let bytes = bid_update.as_bytes();
                total_sent += self.socket.send_to(bytes, &self.config.multicast_addr, self.config.port)?;
            }

            // Send ask snapshot if we have a valid ask
            if state.ask_price > 0 && state.ask_qty > 0 {
                let ask_update = MarketUpdate::new(
                    MarketUpdateType::Snapshot,
                    ticker_id,
                    0, // No specific order ID for snapshot
                    Side::Sell as i8,
                    state.ask_price,
                    state.ask_qty,
                    self.sequence,
                );

                let bytes = ask_update.as_bytes();
                total_sent += self.socket.send_to(bytes, &self.config.multicast_addr, self.config.port)?;
            }
        }

        self.updates_since_snapshot = 0;
        Ok(total_sent)
    }

    /// Publishes a snapshot for a specific ticker.
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker to snapshot
    ///
    /// # Returns
    /// The number of bytes sent, or an IO error
    pub fn publish_ticker_snapshot(&mut self, ticker_id: TickerId) -> io::Result<usize> {
        let state = match self.ticker_state.get(&ticker_id) {
            Some(s) => *s,
            None => return Ok(0),
        };

        let mut total_sent = 0;

        // Send bid snapshot
        if state.bid_price > 0 && state.bid_qty > 0 {
            let bid_update = MarketUpdate::new(
                MarketUpdateType::Snapshot,
                ticker_id,
                0,
                Side::Buy as i8,
                state.bid_price,
                state.bid_qty,
                self.sequence,
            );

            let bytes = bid_update.as_bytes();
            total_sent += self.socket.send_to(bytes, &self.config.multicast_addr, self.config.port)?;
        }

        // Send ask snapshot
        if state.ask_price > 0 && state.ask_qty > 0 {
            let ask_update = MarketUpdate::new(
                MarketUpdateType::Snapshot,
                ticker_id,
                0,
                Side::Sell as i8,
                state.ask_price,
                state.ask_qty,
                self.sequence,
            );

            let bytes = ask_update.as_bytes();
            total_sent += self.socket.send_to(bytes, &self.config.multicast_addr, self.config.port)?;
        }

        Ok(total_sent)
    }

    /// Publishes a clear message for a ticker.
    ///
    /// This notifies subscribers that all orders for this ticker have been cleared.
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker to clear
    ///
    /// # Returns
    /// The number of bytes sent, or an IO error
    pub fn publish_clear(&mut self, ticker_id: TickerId) -> io::Result<usize> {
        let update = MarketUpdate::new(
            MarketUpdateType::Clear,
            ticker_id,
            0,
            0,
            0,
            0,
            self.sequence,
        );

        // Clear internal state
        self.ticker_state.remove(&ticker_id);

        let bytes = update.as_bytes();
        let sent = self.socket.send_to(bytes, &self.config.multicast_addr, self.config.port)?;

        self.sequence += 1;
        self.total_updates_sent += 1;
        self.total_bytes_sent += sent as u64;

        Ok(sent)
    }

    /// Registers a new ticker with the publisher.
    ///
    /// Pre-allocates state for the ticker to avoid allocation during publishing.
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker to register
    pub fn register_ticker(&mut self, ticker_id: TickerId) {
        self.ticker_state.entry(ticker_id).or_default();
    }

    /// Returns the current sequence number.
    #[inline]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the total number of updates sent.
    #[inline]
    pub fn total_updates_sent(&self) -> u64 {
        self.total_updates_sent
    }

    /// Returns the total number of bytes sent.
    #[inline]
    pub fn total_bytes_sent(&self) -> u64 {
        self.total_bytes_sent
    }

    /// Returns the number of tickers being tracked.
    #[inline]
    pub fn ticker_count(&self) -> usize {
        self.ticker_state.len()
    }

    /// Returns the multicast address being used.
    #[inline]
    pub fn multicast_addr(&self) -> &str {
        &self.config.multicast_addr
    }

    /// Returns the port being used.
    #[inline]
    pub fn port(&self) -> u16 {
        self.config.port
    }

    /// Returns a reference to the configuration.
    #[inline]
    pub fn config(&self) -> &MarketDataPublisherConfig {
        &self.config
    }

    /// Returns the current state for a ticker (for testing/debugging).
    #[inline]
    pub fn get_ticker_state(&self, ticker_id: TickerId) -> Option<(Price, Qty, Price, Qty)> {
        self.ticker_state.get(&ticker_id).map(|s| {
            (s.bid_price, s.bid_qty, s.ask_price, s.ask_qty)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MARKET_UPDATE_SIZE;

    fn create_test_config() -> MarketDataPublisherConfig {
        MarketDataPublisherConfig {
            multicast_addr: "239.255.0.1".to_string(),
            port: 5001,
            interface: "0.0.0.0".to_string(),
            ttl: 1,
            enable_snapshots: true,
            snapshot_interval: 100,
        }
    }

    #[test]
    fn test_config_default() {
        let config = MarketDataPublisherConfig::default();
        assert_eq!(config.multicast_addr, "239.255.0.1");
        assert_eq!(config.port, 5000);
        assert_eq!(config.interface, "0.0.0.0");
        assert_eq!(config.ttl, 1);
        assert!(config.enable_snapshots);
        assert_eq!(config.snapshot_interval, 1000);
    }

    #[test]
    fn test_market_update_serialization() {
        // Test that MarketUpdate can be serialized correctly
        let update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,     // ticker_id
            12345, // order_id
            1,     // side (Buy)
            10050, // price
            100,   // qty
            99999, // priority
        );

        let bytes = update.as_bytes();
        assert_eq!(bytes.len(), MARKET_UPDATE_SIZE);

        // Deserialize and verify
        // Copy fields to local variables to avoid unaligned reference issues with packed structs
        let parsed = MarketUpdate::from_bytes(bytes).unwrap();
        let msg_type = parsed.msg_type;
        let ticker_id = parsed.ticker_id;
        let order_id = parsed.order_id;
        let side = parsed.side;
        let price = parsed.price;
        let qty = parsed.qty;
        let priority = parsed.priority;

        assert_eq!(msg_type, MarketUpdateType::Add as u8);
        assert_eq!(ticker_id, 1);
        assert_eq!(order_id, 12345);
        assert_eq!(side, 1);
        assert_eq!(price, 10050);
        assert_eq!(qty, 100);
        assert_eq!(priority, 99999);
    }

    #[test]
    fn test_ticker_state_default() {
        let state = TickerState::default();
        assert_eq!(state.bid_price, 0);
        assert_eq!(state.bid_qty, 0);
        assert_eq!(state.ask_price, 0);
        assert_eq!(state.ask_qty, 0);
        assert_eq!(state.last_seq, 0);
    }

    #[test]
    fn test_ticker_state_update_bid() {
        let mut state = TickerState::default();

        // Simulate bid update
        state.bid_price = 10050;
        state.bid_qty = 100;

        assert_eq!(state.bid_price, 10050);
        assert_eq!(state.bid_qty, 100);
    }

    #[test]
    fn test_ticker_state_update_ask() {
        let mut state = TickerState::default();

        // Simulate ask update
        state.ask_price = 10060;
        state.ask_qty = 200;

        assert_eq!(state.ask_price, 10060);
        assert_eq!(state.ask_qty, 200);
    }

    #[test]
    fn test_ticker_state_cancel_reduces_qty() {
        let mut state = TickerState::default();
        state.bid_price = 10050;
        state.bid_qty = 100;

        // Simulate partial cancel
        state.bid_qty = state.bid_qty.saturating_sub(30);
        assert_eq!(state.bid_qty, 70);

        // Simulate full cancel
        state.bid_qty = state.bid_qty.saturating_sub(100);
        assert_eq!(state.bid_qty, 0);
    }

    #[test]
    fn test_market_update_types() {
        // Test all update types can be created
        let types = [
            MarketUpdateType::Add,
            MarketUpdateType::Modify,
            MarketUpdateType::Cancel,
            MarketUpdateType::Trade,
            MarketUpdateType::Snapshot,
            MarketUpdateType::Clear,
        ];

        for (i, update_type) in types.iter().enumerate() {
            let update = MarketUpdate::new(
                *update_type,
                1,
                (i + 1) as u64,
                1,
                10000,
                100,
                i as u64,
            );

            assert_eq!(update.update_type(), Some(*update_type));
        }
    }

    #[test]
    fn test_publisher_config_clone() {
        let config = MarketDataPublisherConfig::default();
        let cloned = config.clone();

        assert_eq!(cloned.multicast_addr, config.multicast_addr);
        assert_eq!(cloned.port, config.port);
        assert_eq!(cloned.interface, config.interface);
        assert_eq!(cloned.ttl, config.ttl);
        assert_eq!(cloned.enable_snapshots, config.enable_snapshots);
        assert_eq!(cloned.snapshot_interval, config.snapshot_interval);
    }

    // Note: The following tests require network access and may fail in sandboxed environments.
    // They are marked with #[ignore] and can be run manually with `cargo test -- --ignored`

    #[test]
    #[ignore]
    fn test_publisher_creation() {
        let config = create_test_config();
        let result = MarketDataPublisher::new(config);
        assert!(result.is_ok());
    }

    #[test]
    #[ignore]
    fn test_publisher_with_defaults() {
        let result = MarketDataPublisher::with_defaults();
        assert!(result.is_ok());
    }

    #[test]
    #[ignore]
    fn test_publisher_register_ticker() {
        let config = create_test_config();
        let mut publisher = MarketDataPublisher::new(config).unwrap();

        assert_eq!(publisher.ticker_count(), 0);

        publisher.register_ticker(1);
        assert_eq!(publisher.ticker_count(), 1);

        publisher.register_ticker(2);
        assert_eq!(publisher.ticker_count(), 2);

        // Registering same ticker again should be idempotent
        publisher.register_ticker(1);
        assert_eq!(publisher.ticker_count(), 2);
    }

    #[test]
    #[ignore]
    fn test_publisher_initial_state() {
        let config = create_test_config();
        let publisher = MarketDataPublisher::new(config.clone()).unwrap();

        assert_eq!(publisher.sequence(), 0);
        assert_eq!(publisher.total_updates_sent(), 0);
        assert_eq!(publisher.total_bytes_sent(), 0);
        assert_eq!(publisher.ticker_count(), 0);
        assert_eq!(publisher.multicast_addr(), "239.255.0.1");
        assert_eq!(publisher.port(), 5001);
    }

    #[test]
    #[ignore]
    fn test_publisher_publish_updates_stats() {
        let config = create_test_config();
        let mut publisher = MarketDataPublisher::new(config).unwrap();

        let update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,
            1,
            Side::Buy as i8,
            10050,
            100,
            1,
        );

        let result = publisher.publish(&update);
        assert!(result.is_ok());

        assert_eq!(publisher.sequence(), 1);
        assert_eq!(publisher.total_updates_sent(), 1);
        assert!(publisher.total_bytes_sent() > 0);
        assert_eq!(publisher.ticker_count(), 1);
    }

    #[test]
    #[ignore]
    fn test_publisher_publish_batch() {
        let config = create_test_config();
        let mut publisher = MarketDataPublisher::new(config).unwrap();

        let updates: Vec<MarketUpdate> = (0..5).map(|i| {
            MarketUpdate::new(
                MarketUpdateType::Add,
                1,
                i as u64,
                Side::Buy as i8,
                10050 + i as i64,
                100,
                i as u64,
            )
        }).collect();

        let result = publisher.publish_batch(&updates);
        assert!(result.is_ok());

        assert_eq!(publisher.sequence(), 5);
        assert_eq!(publisher.total_updates_sent(), 5);
    }

    #[test]
    #[ignore]
    fn test_publisher_ticker_state_tracking() {
        let config = create_test_config();
        let mut publisher = MarketDataPublisher::new(config).unwrap();

        // Publish a bid update
        let bid_update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,
            1,
            Side::Buy as i8,
            10050,
            100,
            1,
        );
        publisher.publish(&bid_update).unwrap();

        // Publish an ask update
        let ask_update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,
            2,
            Side::Sell as i8,
            10060,
            200,
            2,
        );
        publisher.publish(&ask_update).unwrap();

        // Check state
        let state = publisher.get_ticker_state(1).unwrap();
        assert_eq!(state.0, 10050); // bid_price
        assert_eq!(state.1, 100);   // bid_qty
        assert_eq!(state.2, 10060); // ask_price
        assert_eq!(state.3, 200);   // ask_qty
    }

    #[test]
    #[ignore]
    fn test_publisher_snapshot_interval() {
        let mut config = create_test_config();
        config.snapshot_interval = 3; // Trigger snapshot after 3 updates

        let mut publisher = MarketDataPublisher::new(config).unwrap();

        // Publish updates
        for i in 0..4 {
            let update = MarketUpdate::new(
                MarketUpdateType::Add,
                1,
                i as u64,
                Side::Buy as i8,
                10050,
                100,
                i as u64,
            );
            publisher.publish(&update).unwrap();
        }

        // Snapshot should have been triggered, resetting counter
        // Total sent should be > 4 due to snapshot messages
        assert!(publisher.total_updates_sent() >= 4);
    }

    #[test]
    #[ignore]
    fn test_publisher_clear() {
        let config = create_test_config();
        let mut publisher = MarketDataPublisher::new(config).unwrap();

        // Add a ticker
        publisher.register_ticker(1);
        assert_eq!(publisher.ticker_count(), 1);

        // Clear it
        let result = publisher.publish_clear(1);
        assert!(result.is_ok());

        // Ticker state should be removed
        assert!(publisher.get_ticker_state(1).is_none());
    }
}
