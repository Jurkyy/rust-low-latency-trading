// Lock-free SPSC queue implementation
//
// This is a single-producer single-consumer queue optimized for low-latency
// trading systems. It uses atomic operations with carefully chosen memory
// orderings to ensure correctness while minimizing synchronization overhead.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Cache-line aligned writer index.
/// Separated from reader index to prevent false sharing.
#[repr(align(64))]
struct WriterIndex {
    /// The tail index where the producer writes next.
    /// Only modified by the producer thread.
    tail: AtomicUsize,
}

/// Cache-line aligned reader index.
/// Separated from writer index to prevent false sharing.
#[repr(align(64))]
struct ReaderIndex {
    /// The head index where the consumer reads next.
    /// Only modified by the consumer thread.
    head: AtomicUsize,
}

/// A lock-free single-producer single-consumer (SPSC) queue.
///
/// This queue is designed for scenarios where one thread produces data
/// and another thread consumes it, such as communication between the
/// market data handler and the order book.
///
/// # Type Parameters
/// - `T`: The type of elements stored in the queue
/// - `N`: The capacity of the queue (must be a power of 2)
///
/// # Memory Ordering
/// - Producer uses Release ordering when updating tail
/// - Consumer uses Acquire ordering when reading tail
/// - Consumer uses Release ordering when updating head
/// - Producer uses Acquire ordering when reading head
///
/// # Safety
/// - Only one thread may call `push` (the producer)
/// - Only one thread may call `pop` (the consumer)
/// - Multiple readers of `len`, `is_empty`, `is_full`, `capacity` are safe
pub struct LFQueue<T, const N: usize> {
    /// The storage buffer using UnsafeCell for interior mutability.
    /// MaybeUninit is used because slots may be uninitialized.
    buffer: UnsafeCell<[MaybeUninit<T>; N]>,

    /// Writer index (cache-line aligned)
    writer: WriterIndex,

    /// Reader index (cache-line aligned)
    reader: ReaderIndex,
}

// SAFETY: LFQueue is Send if T is Send because we transfer ownership
// of T values between threads through the queue.
unsafe impl<T: Send, const N: usize> Send for LFQueue<T, N> {}

// SAFETY: LFQueue is Sync if T is Send because:
// - Only one thread writes to tail (producer)
// - Only one thread writes to head (consumer)
// - The atomic operations provide the necessary synchronization
unsafe impl<T: Send, const N: usize> Sync for LFQueue<T, N> {}

impl<T, const N: usize> LFQueue<T, N> {
    /// The mask used for efficient modulo operation (N - 1).
    const MASK: usize = N - 1;

    /// Creates a new empty queue.
    ///
    /// # Panics
    /// Panics if N is not a power of 2 or if N is 0.
    ///
    /// # Example
    /// ```
    /// use common::lf_queue::LFQueue;
    /// let queue: LFQueue<u32, 64> = LFQueue::new();
    /// assert!(queue.is_empty());
    /// ```
    pub fn new() -> Self {
        // Ensure N is a power of 2
        assert!(N > 0 && N.is_power_of_two(), "Capacity must be a power of 2");

        Self {
            // SAFETY: MaybeUninit doesn't require initialization
            buffer: UnsafeCell::new(unsafe {
                MaybeUninit::<[MaybeUninit<T>; N]>::uninit().assume_init()
            }),
            writer: WriterIndex {
                tail: AtomicUsize::new(0),
            },
            reader: ReaderIndex {
                head: AtomicUsize::new(0),
            },
        }
    }

    /// Attempts to push an item onto the queue.
    ///
    /// # Arguments
    /// * `item` - The item to push
    ///
    /// # Returns
    /// * `Ok(())` if the item was successfully pushed
    /// * `Err(item)` if the queue is full, returning ownership of the item
    ///
    /// # Safety
    /// This method must only be called from the producer thread.
    ///
    /// # Example
    /// ```
    /// use common::lf_queue::LFQueue;
    /// let queue: LFQueue<u32, 4> = LFQueue::new();
    /// assert!(queue.push(42).is_ok());
    /// ```
    #[inline]
    pub fn push(&self, item: T) -> Result<(), T> {
        // Load current tail with Relaxed ordering - only we modify it
        let tail = self.writer.tail.load(Ordering::Relaxed);

        // Load head with Acquire ordering to synchronize with consumer's Release
        let head = self.reader.head.load(Ordering::Acquire);

        // Check if queue is full
        // Full when: (tail - head) == N
        if tail.wrapping_sub(head) >= N {
            return Err(item);
        }

        // Calculate the actual index in the buffer
        let index = tail & Self::MASK;

        // SAFETY: We have exclusive write access to this slot because:
        // 1. Only the producer writes to slots between head and tail
        // 2. The consumer only reads slots before updating head
        // 3. We've verified there's space (tail - head < N)
        unsafe {
            let buffer = &mut *self.buffer.get();
            buffer[index].write(item);
        }

        // Update tail with Release ordering to make the write visible to consumer
        self.writer.tail.store(tail.wrapping_add(1), Ordering::Release);

        Ok(())
    }

    /// Attempts to pop an item from the queue.
    ///
    /// # Returns
    /// * `Some(item)` if an item was available
    /// * `None` if the queue is empty
    ///
    /// # Safety
    /// This method must only be called from the consumer thread.
    ///
    /// # Example
    /// ```
    /// use common::lf_queue::LFQueue;
    /// let queue: LFQueue<u32, 4> = LFQueue::new();
    /// queue.push(42).unwrap();
    /// assert_eq!(queue.pop(), Some(42));
    /// ```
    #[inline]
    pub fn pop(&self) -> Option<T> {
        // Load current head with Relaxed ordering - only we modify it
        let head = self.reader.head.load(Ordering::Relaxed);

        // Load tail with Acquire ordering to synchronize with producer's Release
        let tail = self.writer.tail.load(Ordering::Acquire);

        // Check if queue is empty
        if head == tail {
            return None;
        }

        // Calculate the actual index in the buffer
        let index = head & Self::MASK;

        // SAFETY: We have exclusive read access to this slot because:
        // 1. The producer has already written to this slot (tail > head)
        // 2. Only the consumer reads slots and then updates head
        // 3. The producer won't overwrite until we update head
        let item = unsafe {
            let buffer = &*self.buffer.get();
            buffer[index].assume_init_read()
        };

        // Update head with Release ordering to signal to producer that slot is free
        self.reader.head.store(head.wrapping_add(1), Ordering::Release);

        Some(item)
    }

    /// Returns the current number of items in the queue.
    ///
    /// Note: This is an approximation in a concurrent context as the
    /// value may change immediately after reading.
    #[inline]
    pub fn len(&self) -> usize {
        let tail = self.writer.tail.load(Ordering::Relaxed);
        let head = self.reader.head.load(Ordering::Relaxed);
        tail.wrapping_sub(head)
    }

    /// Returns true if the queue is empty.
    ///
    /// Note: This is an approximation in a concurrent context.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns true if the queue is full.
    ///
    /// Note: This is an approximation in a concurrent context.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() >= N
    }

    /// Returns the capacity of the queue.
    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }
}

impl<T, const N: usize> Default for LFQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Drop for LFQueue<T, N> {
    fn drop(&mut self) {
        // Drop any remaining items in the queue
        while self.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_queue_is_empty() {
        let queue: LFQueue<u32, 8> = LFQueue::new();
        assert!(queue.is_empty());
        assert!(!queue.is_full());
        assert_eq!(queue.len(), 0);
        assert_eq!(queue.capacity(), 8);
    }

    #[test]
    fn test_single_push_pop() {
        let queue: LFQueue<u32, 8> = LFQueue::new();

        assert!(queue.push(42).is_ok());
        assert_eq!(queue.len(), 1);
        assert!(!queue.is_empty());

        assert_eq!(queue.pop(), Some(42));
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn test_multiple_push_pop() {
        let queue: LFQueue<u32, 8> = LFQueue::new();

        for i in 0..5 {
            assert!(queue.push(i).is_ok());
        }
        assert_eq!(queue.len(), 5);

        for i in 0..5 {
            assert_eq!(queue.pop(), Some(i));
        }
        assert!(queue.is_empty());
    }

    #[test]
    fn test_pop_empty_returns_none() {
        let queue: LFQueue<u32, 8> = LFQueue::new();
        assert_eq!(queue.pop(), None);
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn test_full_queue_behavior() {
        let queue: LFQueue<u32, 4> = LFQueue::new();

        // Fill the queue
        for i in 0..4 {
            assert!(queue.push(i).is_ok());
        }
        assert!(queue.is_full());
        assert_eq!(queue.len(), 4);

        // Try to push when full - should fail and return the item
        let result = queue.push(100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 100);

        // Queue should still be full with original items
        assert!(queue.is_full());
    }

    #[test]
    fn test_wraparound_behavior() {
        let queue: LFQueue<u32, 4> = LFQueue::new();

        // Fill and empty the queue multiple times to test wraparound
        for round in 0..10 {
            let base = round * 4;

            // Fill
            for i in 0..4 {
                assert!(queue.push(base + i).is_ok(), "Push failed at round {}, i {}", round, i);
            }
            assert!(queue.is_full());

            // Empty
            for i in 0..4 {
                assert_eq!(queue.pop(), Some(base + i), "Pop mismatch at round {}, i {}", round, i);
            }
            assert!(queue.is_empty());
        }
    }

    #[test]
    fn test_interleaved_push_pop() {
        let queue: LFQueue<u32, 4> = LFQueue::new();

        // Interleave pushes and pops
        queue.push(1).unwrap();
        queue.push(2).unwrap();
        assert_eq!(queue.pop(), Some(1));
        queue.push(3).unwrap();
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
        assert!(queue.is_empty());
    }

    #[test]
    fn test_fifo_order() {
        let queue: LFQueue<u32, 8> = LFQueue::new();

        let items: Vec<u32> = (0..8).collect();
        for &item in &items {
            queue.push(item).unwrap();
        }

        for &expected in &items {
            assert_eq!(queue.pop(), Some(expected));
        }
    }

    #[test]
    fn test_with_string_type() {
        let queue: LFQueue<String, 4> = LFQueue::new();

        queue.push("hello".to_string()).unwrap();
        queue.push("world".to_string()).unwrap();

        assert_eq!(queue.pop(), Some("hello".to_string()));
        assert_eq!(queue.pop(), Some("world".to_string()));
        assert!(queue.is_empty());
    }

    #[test]
    fn test_with_struct_type() {
        #[derive(Debug, PartialEq, Clone)]
        struct TestStruct {
            id: u64,
            value: f64,
        }

        let queue: LFQueue<TestStruct, 4> = LFQueue::new();

        let item1 = TestStruct { id: 1, value: 1.5 };
        let item2 = TestStruct { id: 2, value: 2.5 };

        queue.push(item1.clone()).unwrap();
        queue.push(item2.clone()).unwrap();

        assert_eq!(queue.pop(), Some(item1));
        assert_eq!(queue.pop(), Some(item2));
    }

    #[test]
    fn test_capacity_constant() {
        let queue: LFQueue<u32, 16> = LFQueue::new();
        assert_eq!(queue.capacity(), 16);

        let queue2: LFQueue<u32, 256> = LFQueue::new();
        assert_eq!(queue2.capacity(), 256);
    }

    #[test]
    #[should_panic(expected = "Capacity must be a power of 2")]
    fn test_non_power_of_two_panics() {
        let _queue: LFQueue<u32, 5> = LFQueue::new();
    }

    // Note: Zero capacity is a compile-time error due to MASK computation (N - 1 overflow)
    // This is actually desirable behavior - we catch the error at compile time rather than runtime

    #[test]
    fn test_default_trait() {
        let queue: LFQueue<u32, 8> = LFQueue::default();
        assert!(queue.is_empty());
        assert_eq!(queue.capacity(), 8);
    }

    #[test]
    fn test_drop_cleans_up() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

        #[derive(Clone, Debug)]
        struct DropCounter;

        impl Drop for DropCounter {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }

        DROP_COUNT.store(0, Ordering::SeqCst);

        {
            let queue: LFQueue<DropCounter, 4> = LFQueue::new();
            queue.push(DropCounter).unwrap();
            queue.push(DropCounter).unwrap();
            queue.push(DropCounter).unwrap();
            // Queue drops here with 3 items still inside
        }

        assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn test_large_capacity() {
        let queue: LFQueue<u64, 1024> = LFQueue::new();

        for i in 0..1024 {
            assert!(queue.push(i).is_ok());
        }
        assert!(queue.is_full());

        for i in 0..1024 {
            assert_eq!(queue.pop(), Some(i));
        }
        assert!(queue.is_empty());
    }

    #[test]
    fn test_single_element_capacity() {
        let queue: LFQueue<u32, 1> = LFQueue::new();

        assert!(queue.push(42).is_ok());
        assert!(queue.is_full());
        assert!(queue.push(43).is_err());

        assert_eq!(queue.pop(), Some(42));
        assert!(queue.is_empty());

        assert!(queue.push(44).is_ok());
        assert_eq!(queue.pop(), Some(44));
    }
}
