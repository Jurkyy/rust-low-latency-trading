// Message definitions for exchange protocol
//
// Binary message protocol using zerocopy for zero-copy serialization.
// All structs are #[repr(C, packed)] for predictable memory layout.

use zerocopy::{AsBytes, FromBytes, FromZeroes};

// ============================================================================
// Message Type Enums
// ============================================================================

/// Client request types for order submission
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientRequestType {
    New = 1,
    Cancel = 2,
}

impl ClientRequestType {
    /// Convert from raw u8 value
    #[inline]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(ClientRequestType::New),
            2 => Some(ClientRequestType::Cancel),
            _ => None,
        }
    }
}

/// Client response types for order acknowledgments
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientResponseType {
    Accepted = 1,
    Canceled = 2,
    Filled = 3,
    CancelRejected = 4,
    InvalidRequest = 5,
}

impl ClientResponseType {
    /// Convert from raw u8 value
    #[inline]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(ClientResponseType::Accepted),
            2 => Some(ClientResponseType::Canceled),
            3 => Some(ClientResponseType::Filled),
            4 => Some(ClientResponseType::CancelRejected),
            5 => Some(ClientResponseType::InvalidRequest),
            _ => None,
        }
    }
}

/// Market data update types
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketUpdateType {
    Add = 1,
    Modify = 2,
    Cancel = 3,
    Trade = 4,
    Snapshot = 5,
    Clear = 6,
}

impl MarketUpdateType {
    /// Convert from raw u8 value
    #[inline]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(MarketUpdateType::Add),
            2 => Some(MarketUpdateType::Modify),
            3 => Some(MarketUpdateType::Cancel),
            4 => Some(MarketUpdateType::Trade),
            5 => Some(MarketUpdateType::Snapshot),
            6 => Some(MarketUpdateType::Clear),
            _ => None,
        }
    }
}

// ============================================================================
// Message Structs
// ============================================================================

/// Client request message for order submission
///
/// Layout (34 bytes total):
/// - msg_type: u8 (1 byte) - ClientRequestType
/// - client_id: u32 (4 bytes)
/// - ticker_id: u32 (4 bytes)
/// - order_id: u64 (8 bytes)
/// - side: i8 (1 byte) - Side enum value
/// - price: i64 (8 bytes) - fixed-point price in cents
/// - qty: u32 (4 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, AsBytes, FromBytes, FromZeroes)]
pub struct ClientRequest {
    pub msg_type: u8,
    pub client_id: u32,
    pub ticker_id: u32,
    pub order_id: u64,
    pub side: i8,
    pub price: i64,
    pub qty: u32,
}

impl ClientRequest {
    /// Create a new client request
    #[inline]
    pub fn new(
        msg_type: ClientRequestType,
        client_id: u32,
        ticker_id: u32,
        order_id: u64,
        side: i8,
        price: i64,
        qty: u32,
    ) -> Self {
        Self {
            msg_type: msg_type as u8,
            client_id,
            ticker_id,
            order_id,
            side,
            price,
            qty,
        }
    }

    /// Get the message type as enum
    #[inline]
    pub fn request_type(&self) -> Option<ClientRequestType> {
        ClientRequestType::from_u8(self.msg_type)
    }

    /// Get a byte slice reference to this message (zero-copy)
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        AsBytes::as_bytes(self)
    }

    /// Create a reference from a byte slice (zero-copy)
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        FromBytes::ref_from(bytes)
    }

    /// Create a mutable reference from a byte slice (zero-copy)
    #[inline]
    pub fn from_bytes_mut(bytes: &mut [u8]) -> Option<&mut Self> {
        FromBytes::mut_from(bytes)
    }
}

/// Client response message for order acknowledgments
///
/// Layout (47 bytes total):
/// - msg_type: u8 (1 byte) - ClientResponseType
/// - client_id: u32 (4 bytes)
/// - ticker_id: u32 (4 bytes)
/// - client_order_id: u64 (8 bytes)
/// - market_order_id: u64 (8 bytes)
/// - side: i8 (1 byte)
/// - price: i64 (8 bytes)
/// - exec_qty: u32 (4 bytes)
/// - leaves_qty: u32 (4 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, AsBytes, FromBytes, FromZeroes)]
pub struct ClientResponse {
    pub msg_type: u8,
    pub client_id: u32,
    pub ticker_id: u32,
    pub client_order_id: u64,
    pub market_order_id: u64,
    pub side: i8,
    pub price: i64,
    pub exec_qty: u32,
    pub leaves_qty: u32,
}

impl ClientResponse {
    /// Create a new client response
    #[inline]
    pub fn new(
        msg_type: ClientResponseType,
        client_id: u32,
        ticker_id: u32,
        client_order_id: u64,
        market_order_id: u64,
        side: i8,
        price: i64,
        exec_qty: u32,
        leaves_qty: u32,
    ) -> Self {
        Self {
            msg_type: msg_type as u8,
            client_id,
            ticker_id,
            client_order_id,
            market_order_id,
            side,
            price,
            exec_qty,
            leaves_qty,
        }
    }

    /// Get the message type as enum
    #[inline]
    pub fn response_type(&self) -> Option<ClientResponseType> {
        ClientResponseType::from_u8(self.msg_type)
    }

    /// Get a byte slice reference to this message (zero-copy)
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        AsBytes::as_bytes(self)
    }

    /// Create a reference from a byte slice (zero-copy)
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        FromBytes::ref_from(bytes)
    }

    /// Create a mutable reference from a byte slice (zero-copy)
    #[inline]
    pub fn from_bytes_mut(bytes: &mut [u8]) -> Option<&mut Self> {
        FromBytes::mut_from(bytes)
    }
}

/// Market data update message
///
/// Layout (35 bytes total):
/// - msg_type: u8 (1 byte) - MarketUpdateType
/// - ticker_id: u32 (4 bytes)
/// - order_id: u64 (8 bytes)
/// - side: i8 (1 byte)
/// - price: i64 (8 bytes)
/// - qty: u32 (4 bytes)
/// - priority: u64 (8 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, AsBytes, FromBytes, FromZeroes)]
pub struct MarketUpdate {
    pub msg_type: u8,
    pub ticker_id: u32,
    pub order_id: u64,
    pub side: i8,
    pub price: i64,
    pub qty: u32,
    pub priority: u64,
}

impl MarketUpdate {
    /// Create a new market update
    #[inline]
    pub fn new(
        msg_type: MarketUpdateType,
        ticker_id: u32,
        order_id: u64,
        side: i8,
        price: i64,
        qty: u32,
        priority: u64,
    ) -> Self {
        Self {
            msg_type: msg_type as u8,
            ticker_id,
            order_id,
            side,
            price,
            qty,
            priority,
        }
    }

    /// Get the message type as enum
    #[inline]
    pub fn update_type(&self) -> Option<MarketUpdateType> {
        MarketUpdateType::from_u8(self.msg_type)
    }

    /// Get a byte slice reference to this message (zero-copy)
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        AsBytes::as_bytes(self)
    }

    /// Create a reference from a byte slice (zero-copy)
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        FromBytes::ref_from(bytes)
    }

    /// Create a mutable reference from a byte slice (zero-copy)
    #[inline]
    pub fn from_bytes_mut(bytes: &mut [u8]) -> Option<&mut Self> {
        FromBytes::mut_from(bytes)
    }
}

// ============================================================================
// Message Size Constants
// ============================================================================

/// Size of ClientRequest in bytes
pub const CLIENT_REQUEST_SIZE: usize = std::mem::size_of::<ClientRequest>();

/// Size of ClientResponse in bytes
pub const CLIENT_RESPONSE_SIZE: usize = std::mem::size_of::<ClientResponse>();

/// Size of MarketUpdate in bytes
pub const MARKET_UPDATE_SIZE: usize = std::mem::size_of::<MarketUpdate>();

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_request_size() {
        // 1 + 4 + 4 + 8 + 1 + 8 + 4 = 30 bytes
        assert_eq!(CLIENT_REQUEST_SIZE, 30);
    }

    #[test]
    fn test_client_response_size() {
        // 1 + 4 + 4 + 8 + 8 + 1 + 8 + 4 + 4 = 42 bytes
        assert_eq!(CLIENT_RESPONSE_SIZE, 42);
    }

    #[test]
    fn test_market_update_size() {
        // 1 + 4 + 8 + 1 + 8 + 4 + 8 = 34 bytes
        assert_eq!(MARKET_UPDATE_SIZE, 34);
    }

    #[test]
    fn test_client_request_roundtrip() {
        let request = ClientRequest::new(
            ClientRequestType::New,
            100,  // client_id
            1,    // ticker_id
            12345, // order_id
            1,    // side (Buy)
            10050, // price
            100,  // qty
        );

        let bytes = request.as_bytes();
        assert_eq!(bytes.len(), CLIENT_REQUEST_SIZE);

        let parsed = ClientRequest::from_bytes(bytes).unwrap();
        // Copy fields to local variables to avoid unaligned references
        let msg_type = parsed.msg_type;
        let client_id = parsed.client_id;
        let ticker_id = parsed.ticker_id;
        let order_id = parsed.order_id;
        let side = parsed.side;
        let price = parsed.price;
        let qty = parsed.qty;

        assert_eq!(msg_type, ClientRequestType::New as u8);
        assert_eq!(client_id, 100);
        assert_eq!(ticker_id, 1);
        assert_eq!(order_id, 12345);
        assert_eq!(side, 1);
        assert_eq!(price, 10050);
        assert_eq!(qty, 100);
    }

    #[test]
    fn test_client_response_roundtrip() {
        let response = ClientResponse::new(
            ClientResponseType::Accepted,
            100,   // client_id
            1,     // ticker_id
            12345, // client_order_id
            67890, // market_order_id
            1,     // side
            10050, // price
            0,     // exec_qty
            100,   // leaves_qty
        );

        let bytes = response.as_bytes();
        assert_eq!(bytes.len(), CLIENT_RESPONSE_SIZE);

        let parsed = ClientResponse::from_bytes(bytes).unwrap();
        // Copy fields to local variables to avoid unaligned references
        let msg_type = parsed.msg_type;
        let client_id = parsed.client_id;
        let ticker_id = parsed.ticker_id;
        let client_order_id = parsed.client_order_id;
        let market_order_id = parsed.market_order_id;
        let side = parsed.side;
        let price = parsed.price;
        let exec_qty = parsed.exec_qty;
        let leaves_qty = parsed.leaves_qty;

        assert_eq!(msg_type, ClientResponseType::Accepted as u8);
        assert_eq!(client_id, 100);
        assert_eq!(ticker_id, 1);
        assert_eq!(client_order_id, 12345);
        assert_eq!(market_order_id, 67890);
        assert_eq!(side, 1);
        assert_eq!(price, 10050);
        assert_eq!(exec_qty, 0);
        assert_eq!(leaves_qty, 100);
    }

    #[test]
    fn test_market_update_roundtrip() {
        let update = MarketUpdate::new(
            MarketUpdateType::Add,
            1,     // ticker_id
            12345, // order_id
            1,     // side
            10050, // price
            100,   // qty
            99999, // priority
        );

        let bytes = update.as_bytes();
        assert_eq!(bytes.len(), MARKET_UPDATE_SIZE);

        let parsed = MarketUpdate::from_bytes(bytes).unwrap();
        // Copy fields to local variables to avoid unaligned references
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
    fn test_request_type_conversion() {
        assert_eq!(ClientRequestType::from_u8(1), Some(ClientRequestType::New));
        assert_eq!(ClientRequestType::from_u8(2), Some(ClientRequestType::Cancel));
        assert_eq!(ClientRequestType::from_u8(0), None);
        assert_eq!(ClientRequestType::from_u8(255), None);
    }

    #[test]
    fn test_response_type_conversion() {
        assert_eq!(ClientResponseType::from_u8(1), Some(ClientResponseType::Accepted));
        assert_eq!(ClientResponseType::from_u8(2), Some(ClientResponseType::Canceled));
        assert_eq!(ClientResponseType::from_u8(3), Some(ClientResponseType::Filled));
        assert_eq!(ClientResponseType::from_u8(4), Some(ClientResponseType::CancelRejected));
        assert_eq!(ClientResponseType::from_u8(5), Some(ClientResponseType::InvalidRequest));
        assert_eq!(ClientResponseType::from_u8(0), None);
    }

    #[test]
    fn test_market_update_type_conversion() {
        assert_eq!(MarketUpdateType::from_u8(1), Some(MarketUpdateType::Add));
        assert_eq!(MarketUpdateType::from_u8(2), Some(MarketUpdateType::Modify));
        assert_eq!(MarketUpdateType::from_u8(3), Some(MarketUpdateType::Cancel));
        assert_eq!(MarketUpdateType::from_u8(4), Some(MarketUpdateType::Trade));
        assert_eq!(MarketUpdateType::from_u8(5), Some(MarketUpdateType::Snapshot));
        assert_eq!(MarketUpdateType::from_u8(6), Some(MarketUpdateType::Clear));
        assert_eq!(MarketUpdateType::from_u8(0), None);
    }

    #[test]
    fn test_from_bytes_with_wrong_size() {
        let too_small: [u8; 10] = [0; 10];
        assert!(ClientRequest::from_bytes(&too_small).is_none());
        assert!(ClientResponse::from_bytes(&too_small).is_none());
        assert!(MarketUpdate::from_bytes(&too_small).is_none());
    }
}
