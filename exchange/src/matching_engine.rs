// Order routing and matching engine
//
// The matching engine is the core component that:
// 1. Receives client requests from the order server
// 2. Routes orders to the appropriate order book by ticker
// 3. Executes matching logic (price-time priority)
// 4. Generates ClientResponse messages for acknowledgments
// 5. Generates MarketUpdate messages for market data feed

use common::{TickerId, OrderId, ClientId, Price, Qty, Side};
use crate::order_book::OrderBook;
use crate::protocol::{
    ClientRequest, ClientResponse, MarketUpdate,
    ClientRequestType, ClientResponseType, MarketUpdateType,
};
use std::collections::HashMap;

/// The matching engine routes orders to order books and generates responses
pub struct MatchingEngine {
    /// Order books indexed by ticker ID
    order_books: HashMap<TickerId, OrderBook>,
    /// Next order ID to assign (exchange-assigned IDs)
    next_order_id: OrderId,
}

impl MatchingEngine {
    /// Creates a new matching engine with no order books
    pub fn new() -> Self {
        Self {
            order_books: HashMap::new(),
            next_order_id: 1,
        }
    }

    /// Adds a new ticker to the matching engine
    ///
    /// Creates an order book for the given ticker ID.
    /// Does nothing if the ticker already exists.
    pub fn add_ticker(&mut self, ticker_id: TickerId) {
        self.order_books
            .entry(ticker_id)
            .or_insert_with(|| OrderBook::new(ticker_id));
    }

    /// Process a client request and generate responses
    ///
    /// Returns a tuple of:
    /// - ClientResponse: acknowledgment to send back to the client
    /// - Vec<MarketUpdate>: market data updates to broadcast
    pub fn process_request(&mut self, request: &ClientRequest) -> (ClientResponse, Vec<MarketUpdate>) {
        // Extract fields from packed struct to avoid unaligned reference issues
        let msg_type = request.msg_type;

        match ClientRequestType::from_u8(msg_type) {
            Some(ClientRequestType::New) => self.handle_new_order(request),
            Some(ClientRequestType::Cancel) => self.handle_cancel(request),
            None => self.handle_invalid_request(request),
        }
    }

    /// Handle a new order request
    ///
    /// Attempts to add the order to the appropriate order book.
    /// Returns an Accepted response and Add market update on success.
    fn handle_new_order(&mut self, request: &ClientRequest) -> (ClientResponse, Vec<MarketUpdate>) {
        // Extract fields from packed struct
        let client_id = request.client_id;
        let ticker_id = request.ticker_id;
        let client_order_id = request.order_id;
        let side_raw = request.side;
        let price = request.price;
        let qty = request.qty;

        // Validate ticker exists
        let order_book = match self.order_books.get_mut(&ticker_id) {
            Some(book) => book,
            None => {
                // Ticker not found - reject the order
                return self.create_reject_response(
                    client_id,
                    ticker_id,
                    client_order_id,
                    side_raw,
                    price,
                    qty,
                );
            }
        };

        // Parse side
        let side = match side_raw {
            1 => Side::Buy,
            -1 => Side::Sell,
            _ => {
                return self.create_reject_response(
                    client_id,
                    ticker_id,
                    client_order_id,
                    side_raw,
                    price,
                    qty,
                );
            }
        };

        // Assign a market order ID
        let market_order_id = self.next_order_id;
        self.next_order_id += 1;

        // Add order to the book
        let result = order_book.add_order(
            client_id,
            market_order_id,
            side,
            price,
            qty,
        );

        match result {
            Some(_ptr) => {
                // Order accepted
                let response = ClientResponse::new(
                    ClientResponseType::Accepted,
                    client_id,
                    ticker_id,
                    client_order_id,
                    market_order_id,
                    side_raw,
                    price,
                    0,    // exec_qty - no execution yet
                    qty,  // leaves_qty - full quantity remains
                );

                // Generate market update for the new order
                let update = MarketUpdate::new(
                    MarketUpdateType::Add,
                    ticker_id,
                    market_order_id,
                    side_raw,
                    price,
                    qty,
                    market_order_id, // Use order ID as priority for now
                );

                (response, vec![update])
            }
            None => {
                // Failed to add order (pool exhausted or duplicate)
                self.create_reject_response(
                    client_id,
                    ticker_id,
                    client_order_id,
                    side_raw,
                    price,
                    qty,
                )
            }
        }
    }

    /// Handle a cancel order request
    ///
    /// Attempts to cancel an order from the appropriate order book.
    /// Returns Canceled response and Cancel market update on success.
    /// Returns CancelRejected response if order not found.
    fn handle_cancel(&mut self, request: &ClientRequest) -> (ClientResponse, Vec<MarketUpdate>) {
        // Extract fields from packed struct
        let client_id = request.client_id;
        let ticker_id = request.ticker_id;
        let order_id = request.order_id;
        let side_raw = request.side;
        let price = request.price;

        // Validate ticker exists
        let order_book = match self.order_books.get_mut(&ticker_id) {
            Some(book) => book,
            None => {
                // Ticker not found - reject the cancel
                return self.create_cancel_reject_response(
                    client_id,
                    ticker_id,
                    order_id,
                    side_raw,
                    price,
                );
            }
        };

        // Attempt to cancel the order
        match order_book.cancel_order(order_id) {
            Some(canceled_order) => {
                // Order successfully canceled
                let response = ClientResponse::new(
                    ClientResponseType::Canceled,
                    client_id,
                    ticker_id,
                    order_id,
                    order_id, // market_order_id same as client's order_id for cancels
                    canceled_order.side as i8,
                    canceled_order.price,
                    0,                  // exec_qty
                    canceled_order.qty, // leaves_qty (remaining at cancel time)
                );

                // Generate market update for the cancel
                let update = MarketUpdate::new(
                    MarketUpdateType::Cancel,
                    ticker_id,
                    order_id,
                    canceled_order.side as i8,
                    canceled_order.price,
                    canceled_order.qty,
                    canceled_order.priority,
                );

                (response, vec![update])
            }
            None => {
                // Order not found - reject the cancel
                self.create_cancel_reject_response(
                    client_id,
                    ticker_id,
                    order_id,
                    side_raw,
                    price,
                )
            }
        }
    }

    /// Handle an invalid request type
    fn handle_invalid_request(&self, request: &ClientRequest) -> (ClientResponse, Vec<MarketUpdate>) {
        let client_id = request.client_id;
        let ticker_id = request.ticker_id;
        let order_id = request.order_id;
        let side = request.side;
        let price = request.price;
        let qty = request.qty;

        let response = ClientResponse::new(
            ClientResponseType::InvalidRequest,
            client_id,
            ticker_id,
            order_id,
            0, // no market order ID
            side,
            price,
            0,   // exec_qty
            qty, // leaves_qty
        );

        (response, Vec::new())
    }

    /// Create a reject response for a new order
    fn create_reject_response(
        &self,
        client_id: ClientId,
        ticker_id: TickerId,
        client_order_id: OrderId,
        side: i8,
        price: Price,
        qty: Qty,
    ) -> (ClientResponse, Vec<MarketUpdate>) {
        let response = ClientResponse::new(
            ClientResponseType::InvalidRequest,
            client_id,
            ticker_id,
            client_order_id,
            0, // no market order ID assigned
            side,
            price,
            0,   // exec_qty
            qty, // leaves_qty
        );

        (response, Vec::new())
    }

    /// Create a cancel rejected response
    fn create_cancel_reject_response(
        &self,
        client_id: ClientId,
        ticker_id: TickerId,
        order_id: OrderId,
        side: i8,
        price: Price,
    ) -> (ClientResponse, Vec<MarketUpdate>) {
        let response = ClientResponse::new(
            ClientResponseType::CancelRejected,
            client_id,
            ticker_id,
            order_id,
            0, // no market order ID
            side,
            price,
            0, // exec_qty
            0, // leaves_qty
        );

        (response, Vec::new())
    }

    /// Returns a reference to an order book for the given ticker
    #[inline]
    pub fn get_order_book(&self, ticker_id: TickerId) -> Option<&OrderBook> {
        self.order_books.get(&ticker_id)
    }

    /// Returns a mutable reference to an order book for the given ticker
    #[inline]
    pub fn get_order_book_mut(&mut self, ticker_id: TickerId) -> Option<&mut OrderBook> {
        self.order_books.get_mut(&ticker_id)
    }

    /// Returns the number of tickers in the matching engine
    #[inline]
    pub fn ticker_count(&self) -> usize {
        self.order_books.len()
    }

    /// Returns the next order ID that will be assigned
    #[inline]
    pub fn next_order_id(&self) -> OrderId {
        self.next_order_id
    }
}

impl Default for MatchingEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_matching_engine() {
        let engine = MatchingEngine::new();
        assert_eq!(engine.ticker_count(), 0);
        assert_eq!(engine.next_order_id(), 1);
    }

    #[test]
    fn test_add_ticker() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);
        engine.add_ticker(2);

        assert_eq!(engine.ticker_count(), 2);
        assert!(engine.get_order_book(1).is_some());
        assert!(engine.get_order_book(2).is_some());
        assert!(engine.get_order_book(3).is_none());
    }

    #[test]
    fn test_add_duplicate_ticker() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);
        engine.add_ticker(1); // Should be idempotent

        assert_eq!(engine.ticker_count(), 1);
    }

    #[test]
    fn test_new_order_accepted() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        let request = ClientRequest::new(
            ClientRequestType::New,
            100,   // client_id
            1,     // ticker_id
            12345, // order_id
            1,     // side (Buy)
            10050, // price
            100,   // qty
        );

        let (response, updates) = engine.process_request(&request);

        // Copy fields to local variables to avoid unaligned reference issues
        let msg_type = response.msg_type;
        let client_id = response.client_id;
        let ticker_id = response.ticker_id;
        let client_order_id = response.client_order_id;
        let market_order_id = response.market_order_id;
        let side = response.side;
        let price = response.price;
        let exec_qty = response.exec_qty;
        let leaves_qty = response.leaves_qty;

        // Verify response
        assert_eq!(msg_type, ClientResponseType::Accepted as u8);
        assert_eq!(client_id, 100);
        assert_eq!(ticker_id, 1);
        assert_eq!(client_order_id, 12345);
        assert_eq!(market_order_id, 1); // First order ID
        assert_eq!(side, 1);
        assert_eq!(price, 10050);
        assert_eq!(exec_qty, 0);
        assert_eq!(leaves_qty, 100);

        // Verify market update
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

        // Verify order ID incremented
        assert_eq!(engine.next_order_id(), 2);
    }

    #[test]
    fn test_new_order_unknown_ticker() {
        let mut engine = MatchingEngine::new();
        // Don't add any tickers

        let request = ClientRequest::new(
            ClientRequestType::New,
            100,   // client_id
            999,   // ticker_id - doesn't exist
            12345, // order_id
            1,     // side
            10050, // price
            100,   // qty
        );

        let (response, updates) = engine.process_request(&request);

        assert_eq!(response.msg_type, ClientResponseType::InvalidRequest as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_new_order_invalid_side() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        let request = ClientRequest::new(
            ClientRequestType::New,
            100,   // client_id
            1,     // ticker_id
            12345, // order_id
            0,     // side - invalid!
            10050, // price
            100,   // qty
        );

        let (response, updates) = engine.process_request(&request);

        assert_eq!(response.msg_type, ClientResponseType::InvalidRequest as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_cancel_order_not_found() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        let request = ClientRequest::new(
            ClientRequestType::Cancel,
            100,   // client_id
            1,     // ticker_id
            99999, // order_id - doesn't exist
            1,     // side
            10050, // price
            0,     // qty (not used for cancel)
        );

        let (response, updates) = engine.process_request(&request);

        assert_eq!(response.msg_type, ClientResponseType::CancelRejected as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_cancel_order_unknown_ticker() {
        let mut engine = MatchingEngine::new();
        // Don't add any tickers

        let request = ClientRequest::new(
            ClientRequestType::Cancel,
            100,   // client_id
            999,   // ticker_id - doesn't exist
            12345, // order_id
            1,     // side
            10050, // price
            0,     // qty
        );

        let (response, updates) = engine.process_request(&request);

        assert_eq!(response.msg_type, ClientResponseType::CancelRejected as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_invalid_request_type() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        // Create a request with invalid msg_type
        let request = ClientRequest {
            msg_type: 255, // Invalid type
            client_id: 100,
            ticker_id: 1,
            order_id: 12345,
            side: 1,
            price: 10050,
            qty: 100,
        };

        let (response, updates) = engine.process_request(&request);

        assert_eq!(response.msg_type, ClientResponseType::InvalidRequest as u8);
        assert!(updates.is_empty());
    }

    #[test]
    fn test_multiple_orders_increment_id() {
        let mut engine = MatchingEngine::new();
        engine.add_ticker(1);

        for i in 0..5 {
            let request = ClientRequest::new(
                ClientRequestType::New,
                100,
                1,
                i as u64,
                1,
                10050 + i as i64,
                100,
            );

            let (response, _) = engine.process_request(&request);
            let market_order_id = response.market_order_id;
            assert_eq!(market_order_id, (i + 1) as u64);
        }

        assert_eq!(engine.next_order_id(), 6);
    }

    #[test]
    fn test_default_impl() {
        let engine = MatchingEngine::default();
        assert_eq!(engine.ticker_count(), 0);
        assert_eq!(engine.next_order_id(), 1);
    }
}
