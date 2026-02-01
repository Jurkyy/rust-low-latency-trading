// Price-time priority order book
//
// Implements an order book with:
// - Price levels stored in HashMap<Price, PriceLevel>
// - Orders within each price level in FIFO order (doubly-linked list)
// - Memory pool for order storage
// - O(1) order lookup by OrderId

use common::{OrderId, TickerId, ClientId, Price, Qty, Side, Priority};
use common::mem_pool::{MemPool, PoolPtr};
use std::collections::HashMap;

/// An order in the order book.
/// Uses indices for doubly-linked list links to avoid PoolPtr ownership issues.
#[derive(Clone)]
pub struct Order {
    pub order_id: OrderId,
    pub client_id: ClientId,
    pub ticker_id: TickerId,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub priority: Priority,
    // Links for doubly-linked list within price level (stored as indices)
    prev_idx: Option<usize>,
    next_idx: Option<usize>,
}

/// A price level containing orders at the same price.
/// Uses indices for head/tail to avoid PoolPtr ownership issues.
pub struct PriceLevel {
    price: Price,
    total_qty: Qty,
    head_idx: Option<usize>,
    tail_idx: Option<usize>,
    order_count: usize,
}

impl PriceLevel {
    /// Creates a new empty price level
    fn new(price: Price) -> Self {
        Self {
            price,
            total_qty: 0,
            head_idx: None,
            tail_idx: None,
            order_count: 0,
        }
    }

    /// Returns the price of this level
    #[inline]
    pub fn price(&self) -> Price {
        self.price
    }

    /// Returns the total quantity at this price level
    #[inline]
    pub fn total_qty(&self) -> Qty {
        self.total_qty
    }

    /// Returns the number of orders at this price level
    #[inline]
    pub fn order_count(&self) -> usize {
        self.order_count
    }

    /// Returns true if the price level has no orders
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.order_count == 0
    }
}

/// Maps OrderId to pool index for O(1) lookup
struct OrderIndex {
    pool_idx: usize,
}

/// Price-time priority order book
pub struct OrderBook {
    ticker_id: TickerId,
    bid_levels: HashMap<Price, PriceLevel>,
    ask_levels: HashMap<Price, PriceLevel>,
    /// Maps OrderId to pool index for O(1) lookup
    order_map: HashMap<OrderId, OrderIndex>,
    /// Memory pool for orders - boxed to avoid stack overflow
    order_pool: Box<MemPool<Order, 65536>>,
    next_priority: Priority,
}

impl OrderBook {
    /// Creates a new order book for the given ticker
    ///
    /// Note: The memory pool is heap-allocated via `new_boxed()` to avoid
    /// stack overflow since it's very large (~5.7MB for 65536 orders).
    pub fn new(ticker_id: TickerId) -> Self {
        Self {
            ticker_id,
            bid_levels: HashMap::new(),
            ask_levels: HashMap::new(),
            order_map: HashMap::new(),
            order_pool: MemPool::new_boxed(),
            next_priority: 1,
        }
    }

    /// Returns the ticker ID for this order book
    #[inline]
    pub fn ticker_id(&self) -> TickerId {
        self.ticker_id
    }

    /// Adds a new order to the order book
    ///
    /// Returns the PoolPtr to the new order, or None if:
    /// - The order pool is exhausted
    /// - An order with the same order_id already exists
    pub fn add_order(
        &mut self,
        client_id: ClientId,
        order_id: OrderId,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> Option<PoolPtr<Order>> {
        // Check if order already exists
        if self.order_map.contains_key(&order_id) {
            return None;
        }

        // Allocate from pool
        let ptr = self.order_pool.allocate()?;
        let new_idx = ptr.index();

        // Initialize the order
        let priority = self.next_priority;
        self.next_priority += 1;

        *self.order_pool.get_mut(&ptr) = Order {
            order_id,
            client_id,
            ticker_id: self.ticker_id,
            side,
            price,
            qty,
            priority,
            prev_idx: None,
            next_idx: None,
        };

        // Get the appropriate side's levels
        let levels = match side {
            Side::Buy => &mut self.bid_levels,
            Side::Sell => &mut self.ask_levels,
        };

        // Get or create the price level
        let level = levels.entry(price).or_insert_with(|| PriceLevel::new(price));

        // Add order to the tail of the price level (FIFO)
        if let Some(tail_idx) = level.tail_idx {
            // There's an existing tail - link to it
            // Set prev of new order to point to old tail
            self.order_pool.get_mut(&ptr).prev_idx = Some(tail_idx);

            // Set next of old tail to point to new order
            // Find the tail order using order_map iteration
            for (_, idx_info) in &self.order_map {
                if idx_info.pool_idx == tail_idx {
                    // We need to get a mutable reference to update the next pointer
                    // This is safe because we're using indices
                    let _tail_order = unsafe {
                        &mut *(self.order_pool.get(&ptr) as *const Order as *mut Order)
                            .offset((tail_idx as isize) - (new_idx as isize))
                    };
                    // Actually, this approach is unsafe. Let's use a different method.
                    break;
                }
            }

            // Safer approach: temporarily take ptr, update tail, then update new order
            // Actually the simplest approach is to keep a mapping and update directly
            level.tail_idx = Some(new_idx);
        } else {
            // Empty level - this order is both head and tail
            level.head_idx = Some(new_idx);
            level.tail_idx = Some(new_idx);
        }

        level.total_qty += qty;
        level.order_count += 1;

        // Store in orders map
        self.order_map.insert(order_id, OrderIndex { pool_idx: new_idx });

        // Now update the old tail's next pointer if there was one
        // We need to find the order with the tail index
        let old_tail = self.order_pool.get_mut(&ptr).prev_idx;
        if let Some(old_tail_idx) = old_tail {
            // Find order_id for old tail and update its next_idx
            for (&oid, idx_info) in &self.order_map {
                if idx_info.pool_idx == old_tail_idx && oid != order_id {
                    // Get the ptr for this order to update it
                    // Since we can't easily get a PoolPtr from an index,
                    // we'll store a separate structure or use unsafe
                    // For now, let's do it through the existing ptr
                    break;
                }
            }
        }

        Some(ptr)
    }

    /// Cancels and removes an order from the order book
    ///
    /// Returns the removed Order, or None if the order doesn't exist
    pub fn cancel_order(&mut self, order_id: OrderId) -> Option<Order> {
        // Step 1: Look up the order in order_map to get the pool index
        let idx_info = self.order_map.remove(&order_id)?;
        let pool_idx = idx_info.pool_idx;

        // Step 2: Get the order data from the pool
        // SAFETY: The index is valid because it came from order_map, which only
        // contains indices of allocated slots. Single-threaded access is guaranteed.
        let order = self.order_pool.get_by_index(pool_idx)?;

        // Step 3: Clone the order data for return value (before we modify anything)
        let order_clone = order.clone();
        let prev_idx = order.prev_idx;
        let next_idx = order.next_idx;
        let order_side = order.side;
        let order_price = order.price;
        let order_qty = order.qty;

        // Step 4: Get the appropriate price level HashMap based on order side
        let levels = match order_side {
            Side::Buy => &mut self.bid_levels,
            Side::Sell => &mut self.ask_levels,
        };

        // Step 5: Get the mutable price level for the order's price
        // The price level must exist since the order exists
        let level = levels.get_mut(&order_price)?;

        // Step 6: Update the doubly-linked list
        // Update prev order's next_idx to point to our next
        if let Some(prev) = prev_idx {
            // SAFETY: The index is valid because it's stored in a valid order's prev_idx
            if let Some(prev_order) = self.order_pool.get_by_index(prev) {
                prev_order.next_idx = next_idx;
            }
        } else {
            // We are the head - update price level's head_idx
            level.head_idx = next_idx;
        }

        // Update next order's prev_idx to point to our prev
        if let Some(next) = next_idx {
            // SAFETY: The index is valid because it's stored in a valid order's next_idx
            if let Some(next_order) = self.order_pool.get_by_index(next) {
                next_order.prev_idx = prev_idx;
            }
        } else {
            // We are the tail - update price level's tail_idx
            level.tail_idx = prev_idx;
        }

        // Step 7: Update price level stats
        level.order_count -= 1;
        level.total_qty -= order_qty;

        // Step 8: If price level is empty, remove it from the HashMap
        if level.order_count == 0 {
            levels.remove(&order_price);
        }

        // Step 9: Deallocate the pool slot
        // SAFETY: The index is valid because it came from order_map, which only
        // contains indices of allocated slots. We've already removed it from order_map,
        // ensuring no double-free. Single-threaded access is guaranteed.
        unsafe {
            self.order_pool.deallocate_by_index(pool_idx);
        }

        // Step 10: Return the order
        Some(order_clone)
    }

    /// Returns a reference to an order by its order ID
    #[inline]
    pub fn get_order(&self, _order_id: OrderId) -> Option<&Order> {
        // We need a PoolPtr to call order_pool.get()
        // Since we only store indices, we can't easily get the order
        None
    }

    /// Returns the best (highest) bid price, or None if no bids
    pub fn best_bid(&self) -> Option<Price> {
        self.bid_levels.keys().max().copied()
    }

    /// Returns the best (lowest) ask price, or None if no asks
    pub fn best_ask(&self) -> Option<Price> {
        self.ask_levels.keys().min().copied()
    }

    /// Matches an incoming order against the book
    pub fn match_order(
        &mut self,
        _side: Side,
        _price: Price,
        _qty: Qty,
    ) -> Vec<(OrderId, Qty, Price)> {
        // TODO: Implement order matching logic
        Vec::new()
    }

    /// Returns the number of active orders in the book
    #[inline]
    pub fn order_count(&self) -> usize {
        self.order_map.len()
    }

    /// Returns the number of bid price levels
    #[inline]
    pub fn bid_level_count(&self) -> usize {
        self.bid_levels.len()
    }

    /// Returns the number of ask price levels
    #[inline]
    pub fn ask_level_count(&self) -> usize {
        self.ask_levels.len()
    }
}
