//! Multicast socket wrapper for low-latency market data reception.
//!
//! Provides UDP multicast functionality with pre-allocated buffers for
//! zero-allocation I/O, suitable for high-frequency market data feeds.

use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// Buffer size for receive operations (64KB).
const BUFFER_SIZE: usize = 65536;

/// A UDP multicast socket wrapper with pre-allocated receive buffer.
pub struct MulticastSocket {
    socket: Socket,
    recv_buffer: [MaybeUninit<u8>; BUFFER_SIZE],
}

impl MulticastSocket {
    /// Creates a new unbound multicast socket.
    ///
    /// The socket is created but not bound or joined to any group.
    /// Use `join_group` for a complete setup.
    pub fn new() -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        // Disable multicast loopback - we don't want to receive our own packets
        socket.set_multicast_loop_v4(false)?;

        Ok(Self {
            socket,
            // SAFETY: MaybeUninit doesn't require initialization
            recv_buffer: unsafe { MaybeUninit::<[MaybeUninit<u8>; BUFFER_SIZE]>::uninit().assume_init() },
        })
    }

    /// Creates a multicast socket and joins the specified group.
    ///
    /// # Arguments
    /// * `addr` - The multicast group address (e.g., "239.255.0.1")
    /// * `port` - The port number to listen on
    /// * `interface` - The local interface IP to use (e.g., "0.0.0.0" for any)
    ///
    /// # Returns
    /// A MulticastSocket joined to the specified group
    pub fn join_group(addr: &str, port: u16, interface: &str) -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

        // Parse addresses
        let multicast_addr: Ipv4Addr = addr
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid multicast address"))?;

        let interface_addr: Ipv4Addr = interface
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid interface address"))?;

        // Validate multicast address
        if !multicast_addr.is_multicast() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Address is not a valid multicast address",
            ));
        }

        // Set socket options
        socket.set_reuse_address(true)?;

        // On Linux, we can also set SO_REUSEPORT for load balancing
        #[cfg(target_os = "linux")]
        socket.set_reuse_port(true)?;

        // Disable multicast loopback
        socket.set_multicast_loop_v4(false)?;

        // Bind to the port on all interfaces
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
        socket.bind(&SocketAddr::V4(bind_addr).into())?;

        // Join the multicast group
        socket.join_multicast_v4(&multicast_addr, &interface_addr)?;

        Ok(Self {
            socket,
            // SAFETY: MaybeUninit doesn't require initialization
            recv_buffer: unsafe { MaybeUninit::<[MaybeUninit<u8>; BUFFER_SIZE]>::uninit().assume_init() },
        })
    }

    /// Sends data to a multicast address.
    ///
    /// # Arguments
    /// * `data` - The data to send
    /// * `addr` - The destination multicast address
    /// * `port` - The destination port
    ///
    /// # Returns
    /// The number of bytes sent
    pub fn send_to(&self, data: &[u8], addr: &str, port: u16) -> io::Result<usize> {
        let dest_addr: Ipv4Addr = addr
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid address"))?;

        let socket_addr = SocketAddr::V4(SocketAddrV4::new(dest_addr, port));
        self.socket.send_to(data, &socket_addr.into())
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

    /// Sets the socket to non-blocking or blocking mode.
    ///
    /// # Arguments
    /// * `nonblocking` - If true, socket operations will not block
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.socket.set_nonblocking(nonblocking)
    }

    /// Sets the multicast TTL (time-to-live).
    ///
    /// # Arguments
    /// * `ttl` - The TTL value (1 = local network only)
    pub fn set_multicast_ttl(&self, ttl: u32) -> io::Result<()> {
        self.socket.set_multicast_ttl_v4(ttl)
    }

    /// Sets the outgoing interface for multicast packets.
    ///
    /// # Arguments
    /// * `interface` - The local interface IP address
    pub fn set_multicast_interface(&self, interface: &str) -> io::Result<()> {
        let interface_addr: Ipv4Addr = interface
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid interface address"))?;

        self.socket.set_multicast_if_v4(&interface_addr)
    }

    /// Leaves a multicast group.
    ///
    /// # Arguments
    /// * `addr` - The multicast group address to leave
    /// * `interface` - The local interface IP address
    pub fn leave_group(&self, addr: &str, interface: &str) -> io::Result<()> {
        let multicast_addr: Ipv4Addr = addr
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid multicast address"))?;

        let interface_addr: Ipv4Addr = interface
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid interface address"))?;

        self.socket.leave_multicast_v4(&multicast_addr, &interface_addr)
    }

    /// Returns a reference to the underlying socket.
    pub fn socket(&self) -> &Socket {
        &self.socket
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multicast_socket_new() {
        let socket = MulticastSocket::new();
        assert!(socket.is_ok());
    }

    #[test]
    fn test_multicast_socket_nonblocking() {
        let socket = MulticastSocket::new().unwrap();
        assert!(socket.set_nonblocking(true).is_ok());
        assert!(socket.set_nonblocking(false).is_ok());
    }

    #[test]
    fn test_invalid_multicast_address() {
        // 192.168.1.1 is not a multicast address
        let result = MulticastSocket::join_group("192.168.1.1", 5000, "0.0.0.0");
        assert!(result.is_err());
    }
}
