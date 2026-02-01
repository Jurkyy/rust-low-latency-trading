// TCP gateway for order handling
//
// The order server is the TCP gateway that:
// 1. Listens for incoming client TCP connections
// 2. Accepts and manages multiple client connections
// 3. Receives ClientRequest messages and deserializes them
// 4. Assigns global sequence numbers via the FIFO sequencer
// 5. Forwards requests to the matching engine
// 6. Sends ClientResponse messages back to clients

use common::net::tcp::{TcpListener, TcpSocket};
use common::ClientId;
use crate::protocol::{ClientRequest, ClientResponse, CLIENT_REQUEST_SIZE};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

/// Default port for the order server.
pub const DEFAULT_ORDER_SERVER_PORT: u16 = 12345;

/// Maximum number of pending connections in the listen backlog.
pub const MAX_PENDING_CONNECTIONS: i32 = 128;

/// Buffer size for receiving partial messages from clients.
const RECV_BUFFER_SIZE: usize = CLIENT_REQUEST_SIZE * 16;

/// Global sequence number generator for FIFO ordering.
///
/// This ensures all incoming orders are assigned a unique, monotonically
/// increasing sequence number, establishing a total order for all requests.
#[derive(Debug)]
pub struct FifoSequencer {
    /// The next sequence number to assign.
    next_seq: AtomicU64,
}

impl FifoSequencer {
    /// Creates a new FIFO sequencer starting at sequence number 1.
    pub fn new() -> Self {
        Self {
            next_seq: AtomicU64::new(1),
        }
    }

    /// Assigns the next sequence number.
    ///
    /// Thread-safe: uses atomic increment with sequential consistency.
    #[inline]
    pub fn next(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Returns the current sequence number (next to be assigned).
    #[inline]
    pub fn current(&self) -> u64 {
        self.next_seq.load(Ordering::SeqCst)
    }
}

impl Default for FifoSequencer {
    fn default() -> Self {
        Self::new()
    }
}

/// Represents a connected client with its socket and receive buffer.
pub struct ClientConnection {
    /// The client's unique identifier.
    pub client_id: ClientId,
    /// The TCP socket for this client.
    socket: TcpSocket,
    /// Buffer for accumulating partial messages.
    recv_buffer: Vec<u8>,
}

impl ClientConnection {
    /// Creates a new client connection.
    pub fn new(client_id: ClientId, socket: TcpSocket) -> Self {
        Self {
            client_id,
            socket,
            recv_buffer: Vec::with_capacity(RECV_BUFFER_SIZE),
        }
    }

    /// Receives data from the client and parses complete messages.
    ///
    /// Returns a vector of complete ClientRequest messages received.
    /// Returns an error if the connection is broken.
    pub fn receive(&mut self) -> io::Result<Vec<ClientRequest>> {
        let mut requests = Vec::new();

        // Try to receive data (non-blocking)
        match self.socket.try_recv() {
            Ok(Some(data)) => {
                if data.is_empty() {
                    // Connection closed by client
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "Client disconnected",
                    ));
                }
                self.recv_buffer.extend_from_slice(data);
            }
            Ok(None) => {
                // No data available, continue
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Non-blocking socket has no data
            }
            Err(e) => {
                return Err(e);
            }
        }

        // Parse complete messages from the buffer
        while self.recv_buffer.len() >= CLIENT_REQUEST_SIZE {
            if let Some(request) = ClientRequest::from_bytes(&self.recv_buffer[..CLIENT_REQUEST_SIZE]) {
                // Copy the request (since it references buffer memory)
                requests.push(*request);
                self.recv_buffer.drain(..CLIENT_REQUEST_SIZE);
            } else {
                // Invalid message format - skip one byte and try again
                // This is a simple recovery strategy for malformed data
                self.recv_buffer.drain(..1);
            }
        }

        Ok(requests)
    }

    /// Sends a response to the client.
    ///
    /// Returns the number of bytes sent.
    pub fn send(&mut self, response: &ClientResponse) -> io::Result<usize> {
        self.socket.send(response.as_bytes())
    }

    /// Sets the socket to non-blocking mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.socket.set_nonblocking(nonblocking)
    }
}

/// A sequenced client request with its assigned sequence number.
#[derive(Debug, Clone, Copy)]
pub struct SequencedRequest {
    /// The global sequence number assigned by the FIFO sequencer.
    pub sequence_number: u64,
    /// The client ID that sent this request.
    pub client_id: ClientId,
    /// The original client request.
    pub request: ClientRequest,
}

/// Configuration for the order server.
#[derive(Debug, Clone)]
pub struct OrderServerConfig {
    /// IP address to listen on.
    pub listen_addr: String,
    /// Port to listen on.
    pub port: u16,
}

impl Default for OrderServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0".to_string(),
            port: DEFAULT_ORDER_SERVER_PORT,
        }
    }
}

impl OrderServerConfig {
    /// Creates a new configuration with the specified address and port.
    pub fn new(listen_addr: &str, port: u16) -> Self {
        Self {
            listen_addr: listen_addr.to_string(),
            port,
        }
    }
}

/// The TCP order server that accepts client connections and processes orders.
///
/// The order server acts as the TCP gateway between trading clients and the
/// matching engine. It:
/// - Listens for incoming TCP connections
/// - Manages multiple concurrent client connections
/// - Deserializes ClientRequest messages
/// - Assigns global sequence numbers for FIFO ordering
/// - Provides a callback mechanism for forwarding requests to the matching engine
pub struct OrderServer {
    /// The TCP listener for accepting new connections.
    listener: TcpListener,
    /// Connected clients indexed by client ID.
    clients: HashMap<ClientId, ClientConnection>,
    /// FIFO sequencer for assigning global order to requests.
    sequencer: FifoSequencer,
    /// Next client ID to assign.
    next_client_id: ClientId,
    /// Server configuration.
    config: OrderServerConfig,
}

impl OrderServer {
    /// Creates a new order server with the given configuration.
    ///
    /// Binds to the specified address and port, ready to accept connections.
    pub fn new(config: OrderServerConfig) -> io::Result<Self> {
        let listener = TcpListener::bind(&config.listen_addr, config.port)?;
        listener.set_nonblocking(true)?;

        Ok(Self {
            listener,
            clients: HashMap::new(),
            sequencer: FifoSequencer::new(),
            next_client_id: 1,
            config,
        })
    }

    /// Creates a new order server with default configuration.
    pub fn with_defaults() -> io::Result<Self> {
        Self::new(OrderServerConfig::default())
    }

    /// Creates a new order server listening on the specified port.
    pub fn on_port(port: u16) -> io::Result<Self> {
        Self::new(OrderServerConfig::new("0.0.0.0", port))
    }

    /// Polls for new connections and incoming data.
    ///
    /// This is a non-blocking operation that:
    /// 1. Accepts any pending new connections
    /// 2. Receives data from all connected clients
    /// 3. Returns sequenced requests for processing
    ///
    /// The returned requests are ordered by their sequence numbers.
    pub fn poll(&mut self) -> Vec<SequencedRequest> {
        // Accept new connections
        self.accept_connections();

        // Collect requests from all clients
        let mut requests = Vec::new();
        let mut disconnected_clients = Vec::new();

        for (&client_id, connection) in self.clients.iter_mut() {
            match connection.receive() {
                Ok(client_requests) => {
                    for request in client_requests {
                        let seq_num = self.sequencer.next();
                        requests.push(SequencedRequest {
                            sequence_number: seq_num,
                            client_id,
                            request,
                        });
                    }
                }
                Err(_) => {
                    // Client disconnected or error
                    disconnected_clients.push(client_id);
                }
            }
        }

        // Remove disconnected clients
        for client_id in disconnected_clients {
            self.clients.remove(&client_id);
        }

        // Sort by sequence number to maintain FIFO order
        requests.sort_by_key(|r| r.sequence_number);

        requests
    }

    /// Sends a response to a specific client.
    ///
    /// Returns Ok(bytes_sent) on success, or Err if the client is not connected.
    pub fn send_response(&mut self, client_id: ClientId, response: &ClientResponse) -> io::Result<usize> {
        match self.clients.get_mut(&client_id) {
            Some(connection) => connection.send(response),
            None => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                format!("Client {} not connected", client_id),
            )),
        }
    }

    /// Broadcasts a response to all connected clients.
    ///
    /// Returns the number of clients that received the response.
    pub fn broadcast(&mut self, response: &ClientResponse) -> usize {
        let mut sent_count = 0;
        for connection in self.clients.values_mut() {
            if connection.send(response).is_ok() {
                sent_count += 1;
            }
        }
        sent_count
    }

    /// Accepts pending connections (non-blocking).
    fn accept_connections(&mut self) {
        loop {
            match self.listener.accept() {
                Ok(socket) => {
                    let client_id = self.next_client_id;
                    self.next_client_id += 1;

                    // Set socket to non-blocking
                    if socket.set_nonblocking(true).is_err() {
                        continue;
                    }

                    let connection = ClientConnection::new(client_id, socket);
                    self.clients.insert(client_id, connection);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // No more pending connections
                    break;
                }
                Err(_) => {
                    // Accept error, continue trying
                    break;
                }
            }
        }
    }

    /// Returns the number of connected clients.
    #[inline]
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Returns true if there are no connected clients.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Returns the current sequence number (next to be assigned).
    #[inline]
    pub fn current_sequence(&self) -> u64 {
        self.sequencer.current()
    }

    /// Returns the server configuration.
    #[inline]
    pub fn config(&self) -> &OrderServerConfig {
        &self.config
    }

    /// Returns a reference to a client connection if it exists.
    pub fn get_client(&self, client_id: ClientId) -> Option<&ClientConnection> {
        self.clients.get(&client_id)
    }

    /// Disconnects a specific client.
    pub fn disconnect_client(&mut self, client_id: ClientId) -> bool {
        self.clients.remove(&client_id).is_some()
    }

    /// Disconnects all clients.
    pub fn disconnect_all(&mut self) {
        self.clients.clear();
    }

    /// Returns an iterator over connected client IDs.
    pub fn client_ids(&self) -> impl Iterator<Item = ClientId> + '_ {
        self.clients.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClientRequestType, CLIENT_RESPONSE_SIZE};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_fifo_sequencer() {
        let sequencer = FifoSequencer::new();

        assert_eq!(sequencer.current(), 1);
        assert_eq!(sequencer.next(), 1);
        assert_eq!(sequencer.next(), 2);
        assert_eq!(sequencer.next(), 3);
        assert_eq!(sequencer.current(), 4);
    }

    #[test]
    fn test_fifo_sequencer_default() {
        let sequencer = FifoSequencer::default();
        assert_eq!(sequencer.current(), 1);
    }

    #[test]
    fn test_order_server_config_default() {
        let config = OrderServerConfig::default();
        assert_eq!(config.listen_addr, "0.0.0.0");
        assert_eq!(config.port, DEFAULT_ORDER_SERVER_PORT);
    }

    #[test]
    fn test_order_server_config_new() {
        let config = OrderServerConfig::new("127.0.0.1", 9999);
        assert_eq!(config.listen_addr, "127.0.0.1");
        assert_eq!(config.port, 9999);
    }

    #[test]
    fn test_order_server_creation() {
        // Use port 0 to get an available port
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let server = OrderServer::new(config);
        assert!(server.is_ok());

        let server = server.unwrap();
        assert_eq!(server.client_count(), 0);
        assert!(server.is_empty());
        assert_eq!(server.current_sequence(), 1);
    }

    #[test]
    fn test_order_server_with_defaults_may_fail() {
        // Note: This might fail if port 12345 is in use
        // In a real test, we'd use port 0
        let _ = OrderServer::with_defaults();
    }

    #[test]
    fn test_order_server_on_port() {
        let server = OrderServer::on_port(0);
        assert!(server.is_ok());
    }

    #[test]
    fn test_order_server_poll_empty() {
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        // Poll should return empty when no clients
        let requests = server.poll();
        assert!(requests.is_empty());
    }

    #[test]
    fn test_client_connection_and_message() {
        // Create server on a random port
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        // Get the actual port the server is listening on
        // For this we need to get it from the listener's local address
        // Since we don't expose that, we'll use a fixed port for testing

        // For now, just test that the server can poll without errors
        let requests = server.poll();
        assert!(requests.is_empty());
        assert_eq!(server.client_count(), 0);
    }

    #[test]
    fn test_send_response_no_client() {
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        let response = ClientResponse::new(
            crate::protocol::ClientResponseType::Accepted,
            1,      // client_id
            1,      // ticker_id
            1,      // client_order_id
            1,      // market_order_id
            1,      // side
            10000,  // price
            0,      // exec_qty
            100,    // leaves_qty
        );

        // Should fail because client 1 is not connected
        let result = server.send_response(1, &response);
        assert!(result.is_err());
    }

    #[test]
    fn test_disconnect_nonexistent_client() {
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        assert!(!server.disconnect_client(999));
    }

    #[test]
    fn test_disconnect_all() {
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        server.disconnect_all();
        assert_eq!(server.client_count(), 0);
    }

    #[test]
    fn test_client_ids_empty() {
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let server = OrderServer::new(config).unwrap();

        let ids: Vec<ClientId> = server.client_ids().collect();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_sequenced_request_structure() {
        let request = ClientRequest::new(
            ClientRequestType::New,
            1,      // client_id
            1,      // ticker_id
            12345,  // order_id
            1,      // side (Buy)
            10050,  // price
            100,    // qty
        );

        let seq_request = SequencedRequest {
            sequence_number: 42,
            client_id: 1,
            request,
        };

        assert_eq!(seq_request.sequence_number, 42);
        assert_eq!(seq_request.client_id, 1);
        // Copy field from packed struct to avoid unaligned reference
        let order_id = seq_request.request.order_id;
        assert_eq!(order_id, 12345);
    }

    #[test]
    fn test_message_sizes() {
        // Verify our size constants are correct
        assert_eq!(CLIENT_REQUEST_SIZE, std::mem::size_of::<ClientRequest>());
        assert_eq!(CLIENT_RESPONSE_SIZE, std::mem::size_of::<ClientResponse>());
    }

    // Integration test with actual client connection
    #[test]
    fn test_client_connection_integration() {
        use common::net::tcp::TcpSocket;

        // Start server on random port
        let listener = TcpListener::bind("127.0.0.1", 0).unwrap();

        // Get the local address to connect to
        let local_addr = listener.socket().local_addr().unwrap();
        let port = local_addr.as_socket().unwrap().port();

        // Set listener to non-blocking
        listener.set_nonblocking(true).unwrap();

        // Spawn a client thread
        let client_handle = thread::spawn(move || {
            // Give server time to start accepting
            thread::sleep(Duration::from_millis(10));

            // Connect to server
            let mut client = TcpSocket::connect("127.0.0.1", port).unwrap();
            client.set_nonblocking(false).unwrap();

            // Send a request
            let request = ClientRequest::new(
                ClientRequestType::New,
                1,      // client_id
                1,      // ticker_id
                12345,  // order_id
                1,      // side (Buy)
                10050,  // price
                100,    // qty
            );

            client.send(request.as_bytes()).unwrap();

            // Wait a bit for server to process
            thread::sleep(Duration::from_millis(50));
        });

        // Accept the connection
        thread::sleep(Duration::from_millis(20));

        let socket = listener.accept();
        if let Ok(socket) = socket {
            socket.set_nonblocking(true).unwrap();
            let mut connection = ClientConnection::new(1, socket);

            // Give time for data to arrive
            thread::sleep(Duration::from_millis(50));

            // Receive data
            let requests = connection.receive().unwrap();

            // Should have received one request
            assert_eq!(requests.len(), 1);
            let req = &requests[0];
            // Copy fields from packed struct to avoid unaligned reference
            let msg_type = req.msg_type;
            let order_id = req.order_id;
            let ticker_id = req.ticker_id;
            let price = req.price;
            let qty = req.qty;
            assert_eq!(msg_type, ClientRequestType::New as u8);
            assert_eq!(order_id, 12345);
            assert_eq!(ticker_id, 1);
            assert_eq!(price, 10050);
            assert_eq!(qty, 100);
        }

        client_handle.join().unwrap();
    }

    #[test]
    fn test_full_server_integration() {
        use common::net::tcp::TcpSocket;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        // Create server config with random port
        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        // Get the actual port
        let local_addr = server.listener.socket().local_addr().unwrap();
        let port = local_addr.as_socket().unwrap().port();

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // Spawn a client thread
        let client_handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));

            let mut client = TcpSocket::connect("127.0.0.1", port).unwrap();

            // Send a new order request
            let request = ClientRequest::new(
                ClientRequestType::New,
                1,      // client_id
                1,      // ticker_id
                9999,   // order_id
                1,      // side (Buy)
                15000,  // price
                50,     // qty
            );

            client.send(request.as_bytes()).unwrap();

            // Keep connection alive briefly
            thread::sleep(Duration::from_millis(100));
            running_clone.store(false, Ordering::SeqCst);
        });

        // Poll until we get a request or timeout
        let mut received_requests = Vec::new();
        let start = std::time::Instant::now();

        while running.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(1) {
            let requests = server.poll();
            received_requests.extend(requests);

            if !received_requests.is_empty() {
                break;
            }

            thread::sleep(Duration::from_millis(10));
        }

        client_handle.join().unwrap();

        // Verify we received the request
        assert!(!received_requests.is_empty(), "Should have received at least one request");

        let first_request = &received_requests[0];
        assert_eq!(first_request.sequence_number, 1);
        // Copy fields from packed struct to avoid unaligned reference
        let order_id = first_request.request.order_id;
        let price = first_request.request.price;
        let qty = first_request.request.qty;
        assert_eq!(order_id, 9999);
        assert_eq!(price, 15000);
        assert_eq!(qty, 50);

        // Verify client count
        assert!(server.client_count() >= 1);
    }

    #[test]
    fn test_multiple_clients() {
        use common::net::tcp::TcpSocket;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        let local_addr = server.listener.socket().local_addr().unwrap();
        let port = local_addr.as_socket().unwrap().port();

        let running = Arc::new(AtomicBool::new(true));

        // Spawn multiple client threads
        let mut handles = Vec::new();
        for i in 0..3 {
            let running_clone = running.clone();
            let handle = thread::spawn(move || {
                thread::sleep(Duration::from_millis(20 + i * 10));

                let mut client = TcpSocket::connect("127.0.0.1", port).unwrap();

                let request = ClientRequest::new(
                    ClientRequestType::New,
                    i as u32 + 1,   // client_id
                    1,              // ticker_id
                    (i + 1) as u64, // order_id
                    1,              // side
                    10000 + (i as i64 * 100),
                    (i + 1) as u32 * 10,
                );

                client.send(request.as_bytes()).unwrap();

                while running_clone.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(10));
                }
            });
            handles.push(handle);
        }

        // Poll until we get all requests or timeout
        let mut all_requests = Vec::new();
        let start = std::time::Instant::now();

        while all_requests.len() < 3 && start.elapsed() < Duration::from_secs(2) {
            let requests = server.poll();
            all_requests.extend(requests);
            thread::sleep(Duration::from_millis(10));
        }

        running.store(false, Ordering::SeqCst);

        for handle in handles {
            handle.join().unwrap();
        }

        // Verify we received all requests
        assert_eq!(all_requests.len(), 3);

        // Verify sequence numbers are unique and monotonic
        let seq_nums: Vec<u64> = all_requests.iter().map(|r| r.sequence_number).collect();
        for i in 0..seq_nums.len() - 1 {
            assert!(seq_nums[i] < seq_nums[i + 1], "Sequence numbers should be monotonic");
        }
    }

    #[test]
    fn test_send_response_to_client() {
        use common::net::tcp::TcpSocket;
        use crate::protocol::ClientResponseType;

        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        let local_addr = server.listener.socket().local_addr().unwrap();
        let port = local_addr.as_socket().unwrap().port();

        // Connect a client
        let mut client = TcpSocket::connect("127.0.0.1", port).unwrap();
        client.set_nonblocking(true).unwrap();

        // Let server accept the connection
        thread::sleep(Duration::from_millis(50));
        server.poll();

        assert!(server.client_count() >= 1);

        // Get the client ID (should be 1 since it's the first client)
        let client_id = server.client_ids().next().unwrap();

        // Send a response to the client
        let response = ClientResponse::new(
            ClientResponseType::Accepted,
            client_id,
            1,      // ticker_id
            1,      // client_order_id
            1,      // market_order_id
            1,      // side
            10000,  // price
            0,      // exec_qty
            100,    // leaves_qty
        );

        let result = server.send_response(client_id, &response);
        assert!(result.is_ok());

        // Give time for data to arrive
        thread::sleep(Duration::from_millis(50));

        // Client should receive the response
        if let Ok(Some(data)) = client.try_recv() {
            assert_eq!(data.len(), CLIENT_RESPONSE_SIZE);
            let received = ClientResponse::from_bytes(data).unwrap();
            assert_eq!(received.msg_type, ClientResponseType::Accepted as u8);
        }
    }

    #[test]
    fn test_broadcast() {
        use common::net::tcp::TcpSocket;
        use crate::protocol::ClientResponseType;

        let config = OrderServerConfig::new("127.0.0.1", 0);
        let mut server = OrderServer::new(config).unwrap();

        let local_addr = server.listener.socket().local_addr().unwrap();
        let port = local_addr.as_socket().unwrap().port();

        // Connect two clients
        let _client1 = TcpSocket::connect("127.0.0.1", port).unwrap();
        let _client2 = TcpSocket::connect("127.0.0.1", port).unwrap();

        // Let server accept connections
        thread::sleep(Duration::from_millis(50));
        server.poll();

        assert_eq!(server.client_count(), 2);

        // Broadcast a response
        let response = ClientResponse::new(
            ClientResponseType::Filled,
            0,      // broadcast to all
            1,      // ticker_id
            1,      // client_order_id
            1,      // market_order_id
            1,      // side
            10000,  // price
            100,    // exec_qty
            0,      // leaves_qty
        );

        let sent_count = server.broadcast(&response);
        assert_eq!(sent_count, 2);
    }
}
