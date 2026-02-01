//! Order Gateway for sending orders to the exchange and receiving responses.
//!
//! Provides a low-latency TCP connection to the exchange for order submission
//! and response handling with sequence number tracking.

use common::net::tcp::TcpSocket;
use common::time::{now_nanos, Nanos};
use common::{ClientId, OrderId, Price, Qty, Side, TickerId};
use exchange::protocol::{
    ClientRequest, ClientRequestType, ClientResponse, CLIENT_RESPONSE_SIZE,
};
use std::collections::HashMap;

/// Represents a pending order that has been sent but not yet acknowledged.
#[derive(Debug, Clone)]
pub struct PendingOrder {
    /// The order ID assigned by the client.
    pub order_id: OrderId,
    /// The ticker/instrument ID.
    pub ticker_id: TickerId,
    /// Buy or sell side.
    pub side: Side,
    /// Order price in fixed-point format.
    pub price: Price,
    /// Order quantity.
    pub qty: Qty,
    /// Timestamp when the order was sent (for latency tracking).
    pub sent_time: Nanos,
}

/// Order gateway for communicating with the exchange.
///
/// Handles TCP connection, message serialization, sequence number tracking,
/// and pending order management.
pub struct OrderGateway {
    /// TCP socket connection to the exchange.
    socket: TcpSocket,
    /// Client identifier for this trading session.
    client_id: ClientId,
    /// Next order ID to assign (monotonically increasing).
    next_order_id: OrderId,
    /// Map of pending orders awaiting acknowledgment.
    pending_orders: HashMap<OrderId, PendingOrder>,
    /// Receive buffer for partial message handling.
    recv_buffer: Vec<u8>,
}

impl OrderGateway {
    /// Connects to the exchange at the specified address.
    ///
    /// # Arguments
    /// * `addr` - The IP address or hostname of the exchange
    /// * `port` - The port number to connect to
    /// * `client_id` - The client identifier for this trading session
    ///
    /// # Returns
    /// A connected `OrderGateway` on success, or an IO error on failure
    pub fn connect(addr: &str, port: u16, client_id: ClientId) -> std::io::Result<Self> {
        let socket = TcpSocket::connect(addr, port)?;
        // Set non-blocking mode for polling
        socket.set_nonblocking(true)?;

        Ok(Self {
            socket,
            client_id,
            next_order_id: 1,
            pending_orders: HashMap::new(),
            recv_buffer: Vec::with_capacity(CLIENT_RESPONSE_SIZE * 16),
        })
    }

    /// Sends a new order to the exchange.
    ///
    /// # Arguments
    /// * `ticker_id` - The ticker/instrument to trade
    /// * `side` - Buy or sell
    /// * `price` - The limit price in fixed-point format
    /// * `qty` - The quantity to trade
    ///
    /// # Returns
    /// The order ID assigned to this order
    pub fn send_new_order(
        &mut self,
        ticker_id: TickerId,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> OrderId {
        let order_id = self.next_order_id;
        self.next_order_id += 1;

        let request = ClientRequest::new(
            ClientRequestType::New,
            self.client_id,
            ticker_id,
            order_id,
            side as i8,
            price,
            qty,
        );

        let sent_time = now_nanos();

        // Send the request (ignore partial sends for simplicity in this implementation)
        let _ = self.socket.send(request.as_bytes());

        // Track the pending order
        self.pending_orders.insert(
            order_id,
            PendingOrder {
                order_id,
                ticker_id,
                side,
                price,
                qty,
                sent_time,
            },
        );

        order_id
    }

    /// Sends a cancel request for an existing order.
    ///
    /// # Arguments
    /// * `order_id` - The order ID to cancel
    /// * `ticker_id` - The ticker/instrument of the order
    pub fn send_cancel(&mut self, order_id: OrderId, ticker_id: TickerId) {
        // Get order details if available, otherwise use defaults
        let (side, price, qty) = if let Some(pending) = self.pending_orders.get(&order_id) {
            (pending.side as i8, pending.price, pending.qty)
        } else {
            // Order not in pending map, use placeholder values
            // The exchange should use the order_id to look up the order
            (0, 0, 0)
        };

        let request = ClientRequest::new(
            ClientRequestType::Cancel,
            self.client_id,
            ticker_id,
            order_id,
            side,
            price,
            qty,
        );

        // Send the cancel request
        let _ = self.socket.send(request.as_bytes());
    }

    /// Polls for incoming responses from the exchange.
    ///
    /// This is a non-blocking operation that returns immediately if no data
    /// is available.
    ///
    /// # Returns
    /// `Some(ClientResponse)` if a complete response was received,
    /// `None` if no data is available
    pub fn poll(&mut self) -> Option<ClientResponse> {
        // Try to receive data
        match self.socket.try_recv() {
            Ok(Some(data)) => {
                // Append received data to buffer
                self.recv_buffer.extend_from_slice(data);
            }
            Ok(None) => {
                // No data available
            }
            Err(_) => {
                // Connection error - could log or handle differently
                return None;
            }
        }

        // Check if we have a complete message
        if self.recv_buffer.len() >= CLIENT_RESPONSE_SIZE {
            // Parse the response
            if let Some(response) = ClientResponse::from_bytes(&self.recv_buffer[..CLIENT_RESPONSE_SIZE]) {
                // Copy the response since we're borrowing from the buffer
                let response_copy = *response;

                // Remove the processed message from the buffer
                self.recv_buffer.drain(..CLIENT_RESPONSE_SIZE);

                // Update pending orders based on response
                let client_order_id = response_copy.client_order_id;
                if let Some(response_type) = response_copy.response_type() {
                    use exchange::protocol::ClientResponseType;
                    match response_type {
                        ClientResponseType::Canceled
                        | ClientResponseType::CancelRejected
                        | ClientResponseType::InvalidRequest => {
                            // Remove from pending on terminal states
                            self.pending_orders.remove(&client_order_id);
                        }
                        ClientResponseType::Filled => {
                            // Check if fully filled (leaves_qty == 0)
                            if response_copy.leaves_qty == 0 {
                                self.pending_orders.remove(&client_order_id);
                            }
                        }
                        ClientResponseType::Accepted => {
                            // Order is still pending, keep tracking
                        }
                    }
                }

                return Some(response_copy);
            }
        }

        None
    }

    /// Gets a reference to a pending order by its order ID.
    ///
    /// # Arguments
    /// * `order_id` - The order ID to look up
    ///
    /// # Returns
    /// `Some(&PendingOrder)` if the order is pending, `None` otherwise
    pub fn get_pending(&self, order_id: OrderId) -> Option<&PendingOrder> {
        self.pending_orders.get(&order_id)
    }

    /// Returns the number of pending orders.
    #[inline]
    pub fn pending_count(&self) -> usize {
        self.pending_orders.len()
    }

    /// Returns the client ID for this gateway.
    #[inline]
    pub fn client_id(&self) -> ClientId {
        self.client_id
    }

    /// Returns the next order ID that will be assigned.
    #[inline]
    pub fn next_order_id(&self) -> OrderId {
        self.next_order_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_order_creation() {
        let pending = PendingOrder {
            order_id: 1,
            ticker_id: 100,
            side: Side::Buy,
            price: 10050,
            qty: 100,
            sent_time: Nanos::new(1000000),
        };

        assert_eq!(pending.order_id, 1);
        assert_eq!(pending.ticker_id, 100);
        assert_eq!(pending.side, Side::Buy);
        assert_eq!(pending.price, 10050);
        assert_eq!(pending.qty, 100);
    }
}
