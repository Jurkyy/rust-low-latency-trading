//! TCP socket wrapper for low-latency trading applications.
//!
//! Provides a thin wrapper around socket2 for fine-grained control over TCP sockets
//! with pre-allocated buffers to avoid runtime allocations.

use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::mem::MaybeUninit;
use std::net::{SocketAddr, ToSocketAddrs};

/// Buffer size for send and receive operations (64KB).
const BUFFER_SIZE: usize = 65536;

/// A TCP socket wrapper with pre-allocated buffers for zero-allocation I/O.
pub struct TcpSocket {
    socket: Socket,
    recv_buffer: [MaybeUninit<u8>; BUFFER_SIZE],
    send_buffer: [u8; BUFFER_SIZE],
}

impl TcpSocket {
    /// Creates a new TcpSocket from an existing socket2::Socket.
    fn from_socket(socket: Socket) -> Self {
        Self {
            socket,
            // SAFETY: MaybeUninit doesn't require initialization
            recv_buffer: unsafe { MaybeUninit::<[MaybeUninit<u8>; BUFFER_SIZE]>::uninit().assume_init() },
            send_buffer: [0u8; BUFFER_SIZE],
        }
    }

    /// Connects to a remote address.
    ///
    /// # Arguments
    /// * `addr` - The IP address or hostname to connect to
    /// * `port` - The port number to connect to
    ///
    /// # Returns
    /// A connected TcpSocket on success
    pub fn connect(addr: &str, port: u16) -> io::Result<Self> {
        let address = format!("{}:{}", addr, port);
        let socket_addr: SocketAddr = address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid address"))?;

        let domain = if socket_addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };

        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

        // Set TCP_NODELAY for low latency
        socket.set_nodelay(true)?;

        socket.connect(&socket_addr.into())?;

        Ok(Self::from_socket(socket))
    }

    /// Creates a TCP listener bound to the specified address.
    ///
    /// # Arguments
    /// * `addr` - The IP address to bind to
    /// * `port` - The port number to listen on
    ///
    /// # Returns
    /// A TcpListener ready to accept connections
    pub fn listen(addr: &str, port: u16) -> io::Result<TcpListener> {
        TcpListener::bind(addr, port)
    }

    /// Sets the socket to non-blocking or blocking mode.
    ///
    /// # Arguments
    /// * `nonblocking` - If true, socket operations will not block
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.socket.set_nonblocking(nonblocking)
    }

    /// Enables or disables TCP_NODELAY (Nagle's algorithm).
    ///
    /// # Arguments
    /// * `nodelay` - If true, disables Nagle's algorithm for lower latency
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.socket.set_nodelay(nodelay)
    }

    /// Sends data over the socket.
    ///
    /// # Arguments
    /// * `data` - The data to send
    ///
    /// # Returns
    /// The number of bytes sent
    pub fn send(&mut self, data: &[u8]) -> io::Result<usize> {
        // Copy data to send buffer if it fits, otherwise send directly
        if data.len() <= BUFFER_SIZE {
            self.send_buffer[..data.len()].copy_from_slice(data);
            self.socket.send(&self.send_buffer[..data.len()])
        } else {
            self.socket.send(data)
        }
    }

    /// Receives data from the socket (blocking).
    ///
    /// # Returns
    /// A slice of the received data from the internal buffer
    pub fn recv(&mut self) -> io::Result<&[u8]> {
        let n = self.socket.recv(&mut self.recv_buffer)?;
        // SAFETY: recv() guarantees the first n bytes are initialized
        Ok(unsafe { std::slice::from_raw_parts(self.recv_buffer.as_ptr() as *const u8, n) })
    }

    /// Attempts to receive data without blocking.
    ///
    /// # Returns
    /// - `Ok(Some(&[u8]))` - Data was received
    /// - `Ok(None)` - No data available (would block)
    /// - `Err(e)` - An error occurred
    pub fn try_recv(&mut self) -> io::Result<Option<&[u8]>> {
        // Temporarily set non-blocking
        let was_nonblocking = self.socket.nonblocking()?;
        if !was_nonblocking {
            self.socket.set_nonblocking(true)?;
        }

        let result = self.socket.recv(&mut self.recv_buffer);

        // Restore blocking mode if needed
        if !was_nonblocking {
            self.socket.set_nonblocking(false)?;
        }

        match result {
            // SAFETY: recv() guarantees the first n bytes are initialized
            Ok(n) => Ok(Some(unsafe { std::slice::from_raw_parts(self.recv_buffer.as_ptr() as *const u8, n) })),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Returns a reference to the underlying socket.
    pub fn socket(&self) -> &Socket {
        &self.socket
    }
}

/// A TCP listener that accepts incoming connections.
pub struct TcpListener {
    listener: Socket,
}

impl TcpListener {
    /// Binds to the specified address and starts listening.
    ///
    /// # Arguments
    /// * `addr` - The IP address to bind to
    /// * `port` - The port number to listen on
    ///
    /// # Returns
    /// A TcpListener ready to accept connections
    pub fn bind(addr: &str, port: u16) -> io::Result<Self> {
        let address = format!("{}:{}", addr, port);
        let socket_addr: SocketAddr = address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid address"))?;

        let domain = if socket_addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };

        let listener = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

        // Set SO_REUSEADDR for quick rebinding
        listener.set_reuse_address(true)?;

        listener.bind(&socket_addr.into())?;
        listener.listen(128)?;

        Ok(Self { listener })
    }

    /// Accepts an incoming connection.
    ///
    /// # Returns
    /// A TcpSocket for the accepted connection
    pub fn accept(&self) -> io::Result<TcpSocket> {
        let (socket, _addr) = self.listener.accept()?;

        // Set TCP_NODELAY on accepted socket
        socket.set_nodelay(true)?;

        Ok(TcpSocket::from_socket(socket))
    }

    /// Sets the listener to non-blocking or blocking mode.
    ///
    /// # Arguments
    /// * `nonblocking` - If true, accept() will not block
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.listener.set_nonblocking(nonblocking)
    }

    /// Returns a reference to the underlying socket.
    pub fn socket(&self) -> &Socket {
        &self.listener
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_listener_bind() {
        // Use port 0 to let the OS assign an available port
        let listener = TcpListener::bind("127.0.0.1", 0);
        assert!(listener.is_ok());
    }

    #[test]
    fn test_listener_nonblocking() {
        let listener = TcpListener::bind("127.0.0.1", 0).unwrap();
        assert!(listener.set_nonblocking(true).is_ok());
        assert!(listener.set_nonblocking(false).is_ok());
    }
}
