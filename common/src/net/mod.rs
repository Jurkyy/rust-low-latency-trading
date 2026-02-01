//! Networking utilities for low-latency trading.
//!
//! This module provides TCP and multicast socket wrappers optimized for
//! high-frequency trading applications with pre-allocated buffers and
//! fine-grained socket control.
//!
//! # Modules
//!
//! - [`tcp`] - TCP socket and listener with pre-allocated buffers
//! - [`multicast`] - UDP multicast for market data feeds

pub mod multicast;
pub mod tcp;

pub use multicast::MulticastSocket;
pub use tcp::{TcpListener, TcpSocket};
