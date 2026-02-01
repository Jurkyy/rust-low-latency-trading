// Memory pool allocator
//
// A generic typed memory pool for zero-allocation object management after initialization.
// Designed for single-threaded, low-latency use cases where allocation predictability
// is critical (e.g., per-component pools in trading systems).
//
// # Safety Invariants
//
// This pool uses interior mutability via UnsafeCell for single-threaded performance.
// The following invariants must be maintained:
//
// 1. Single-threaded access only - no concurrent access to the same pool
// 2. PoolPtr must not be cloned - each PoolPtr represents unique ownership of a slot
// 3. A PoolPtr must only be used with the pool that created it
// 4. A PoolPtr must not be used after deallocation (use-after-free)
// 5. Each slot must be deallocated exactly once (no double-free)

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::marker::PhantomData;

/// A pre-allocated pool of N objects of type T.
///
/// Provides O(1) allocation and deallocation with zero heap allocations
/// after initialization. Uses a free-list implemented as a stack of indices.
///
/// # Safety
///
/// This type is designed for **single-threaded use only**. Using it from
/// multiple threads simultaneously is undefined behavior.
///
/// # Example
///
/// ```
/// use common::mem_pool::MemPool;
///
/// let pool: MemPool<u64, 16> = MemPool::new();
///
/// // Allocate a slot
/// let ptr = pool.allocate().expect("pool not exhausted");
///
/// // Write to the slot
/// *pool.get_mut(&ptr) = 42;
///
/// // Read from the slot
/// assert_eq!(*pool.get(&ptr), 42);
///
/// // Return the slot to the pool
/// pool.deallocate(ptr);
/// ```
pub struct MemPool<T, const N: usize> {
    /// Storage for pool objects. Objects are uninitialized until allocated
    /// and written to by the user.
    storage: UnsafeCell<[MaybeUninit<T>; N]>,

    /// Stack of free indices. free_list[0..free_count] contains valid free indices.
    /// The top of the stack is at free_list[free_count - 1].
    free_list: UnsafeCell<[usize; N]>,

    /// Number of available (free) slots. Also serves as the stack pointer
    /// for the free list.
    free_count: UnsafeCell<usize>,
}

/// A pointer to an allocated slot in a MemPool.
///
/// This type does NOT implement Clone to prevent double-free bugs.
/// Each PoolPtr represents unique ownership of a slot in the pool.
///
/// # Safety
///
/// - Must only be used with the pool that created it
/// - Must not be used after being passed to `deallocate()`
/// - Must be deallocated exactly once (or leaked intentionally)
pub struct PoolPtr<T> {
    /// Index into the pool's storage array
    index: usize,

    /// Direct pointer for fast access (avoids index calculation)
    ptr: *mut T,

    /// Marker to tie the lifetime to type T without owning it
    _marker: PhantomData<T>,
}

// PoolPtr is Send if T is Send (can transfer ownership across threads)
// but the pool itself should only be used from one thread
unsafe impl<T: Send> Send for PoolPtr<T> {}

impl<T, const N: usize> MemPool<T, N> {
    /// Creates a new memory pool with all N slots available.
    ///
    /// The storage is uninitialized - objects are only initialized
    /// when the user writes to an allocated slot.
    ///
    /// # Warning
    ///
    /// For large pools (N > 1024), this may cause stack overflow because
    /// the arrays are created on the stack before being returned. Use
    /// `new_boxed()` instead for large pools.
    ///
    /// # Panics
    ///
    /// Panics if N is 0 (a zero-capacity pool is not useful).
    pub fn new() -> Self {
        assert!(N > 0, "MemPool capacity must be greater than 0");

        // Initialize free list with all indices [0, 1, 2, ..., N-1]
        // We use a const fn approach to initialize at compile time where possible
        let mut free_list = [0usize; N];
        let mut i = 0;
        while i < N {
            free_list[i] = i;
            i += 1;
        }

        Self {
            // SAFETY: MaybeUninit doesn't require initialization
            storage: UnsafeCell::new(unsafe {
                MaybeUninit::<[MaybeUninit<T>; N]>::uninit().assume_init()
            }),
            free_list: UnsafeCell::new(free_list),
            free_count: UnsafeCell::new(N),
        }
    }

    /// Creates a new memory pool directly on the heap, avoiding stack overflow
    /// for large pools.
    ///
    /// This is the preferred way to create large pools (N > 1024).
    ///
    /// # Panics
    ///
    /// Panics if N is 0 (a zero-capacity pool is not useful).
    pub fn new_boxed() -> Box<Self> {
        assert!(N > 0, "MemPool capacity must be greater than 0");

        // Use alloc API to allocate zeroed memory directly on heap
        // This avoids any stack allocation of the large arrays
        use std::alloc::{alloc_zeroed, Layout};

        // SAFETY: Layout is valid for MemPool<T, N>
        let layout = Layout::new::<Self>();
        let ptr = unsafe { alloc_zeroed(layout) as *mut Self };

        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        // SAFETY: We allocated valid memory and will initialize it properly.
        // UnsafeCell has the same memory layout as its inner type, so zeroed
        // memory is a valid representation. We just need to write the actual
        // values we need (free_count and free_list indices).
        unsafe {
            // Write free_count value (N) directly into the UnsafeCell's inner value
            // UnsafeCell<usize> has the same layout as usize
            let free_count_inner = std::ptr::addr_of_mut!((*ptr).free_count) as *mut usize;
            std::ptr::write(free_count_inner, N);

            // Storage is zeroed which is fine - MaybeUninit doesn't require initialization
            // The UnsafeCell wrapper is transparent in memory layout

            // Initialize free_list with indices 0..N
            // UnsafeCell<[usize; N]> has same layout as [usize; N]
            let free_list_inner = std::ptr::addr_of_mut!((*ptr).free_list) as *mut [usize; N];
            for i in 0..N {
                (*free_list_inner)[i] = i;
            }

            Box::from_raw(ptr)
        }
    }

    /// Allocates a slot from the pool.
    ///
    /// Returns `Some(PoolPtr)` if a slot is available, `None` if the pool is exhausted.
    /// The allocated slot contains uninitialized memory - the caller must initialize
    /// it before reading.
    ///
    /// # Safety
    ///
    /// This method uses interior mutability. Caller must ensure single-threaded access.
    ///
    /// # Performance
    ///
    /// O(1) - simply pops from the free list stack.
    #[inline]
    pub fn allocate(&self) -> Option<PoolPtr<T>> {
        // SAFETY: Single-threaded access is required by the type's contract
        unsafe {
            let free_count = &mut *self.free_count.get();

            if *free_count == 0 {
                return None;
            }

            // Pop index from free list stack
            *free_count -= 1;
            let free_list = &*self.free_list.get();
            let index = free_list[*free_count];

            // Get pointer to the storage slot
            let storage = &mut *self.storage.get();
            let ptr = storage[index].as_mut_ptr();

            Some(PoolPtr {
                index,
                ptr,
                _marker: PhantomData,
            })
        }
    }

    /// Returns a slot to the pool.
    ///
    /// After this call, the PoolPtr is consumed and must not be used again.
    /// The slot becomes available for future allocations.
    ///
    /// # Safety
    ///
    /// - The PoolPtr must have been allocated from this pool
    /// - The PoolPtr must not have been previously deallocated (no double-free)
    /// - After deallocation, any references obtained via get/get_mut are invalidated
    ///
    /// # Performance
    ///
    /// O(1) - simply pushes to the free list stack.
    ///
    /// # Note
    ///
    /// This does NOT drop the value at the slot. If T requires cleanup,
    /// the caller must explicitly drop it before deallocating:
    ///
    /// ```ignore
    /// unsafe { std::ptr::drop_in_place(pool.get_mut(&ptr)) };
    /// pool.deallocate(ptr);
    /// ```
    #[inline]
    pub fn deallocate(&self, ptr: PoolPtr<T>) {
        debug_assert!(ptr.index < N, "PoolPtr index out of bounds - wrong pool?");

        // SAFETY: Single-threaded access is required by the type's contract
        unsafe {
            let free_count = &mut *self.free_count.get();
            let free_list = &mut *self.free_list.get();

            debug_assert!(
                *free_count < N,
                "Double-free detected: pool already has all slots free"
            );

            // Push index back onto free list stack
            free_list[*free_count] = ptr.index;
            *free_count += 1;
        }

        // ptr is consumed here, preventing reuse
    }

    /// Returns a shared reference to the object at the given slot.
    ///
    /// # Safety
    ///
    /// - The PoolPtr must have been allocated from this pool and not yet deallocated
    /// - The slot must have been initialized (written to) before reading
    /// - No mutable reference to the same slot must exist
    #[inline]
    pub fn get(&self, ptr: &PoolPtr<T>) -> &T {
        debug_assert!(ptr.index < N, "PoolPtr index out of bounds");

        // SAFETY: Caller guarantees the slot is allocated, initialized,
        // and no mutable references exist
        unsafe { &*ptr.ptr }
    }

    /// Returns a mutable reference to the object at the given slot.
    ///
    /// # Safety
    ///
    /// - The PoolPtr must have been allocated from this pool and not yet deallocated
    /// - No other references (shared or mutable) to the same slot must exist
    #[inline]
    pub fn get_mut(&self, ptr: &PoolPtr<T>) -> &mut T {
        debug_assert!(ptr.index < N, "PoolPtr index out of bounds");

        // SAFETY: Caller guarantees the slot is allocated and no other
        // references exist. Interior mutability is used intentionally.
        unsafe { &mut *ptr.ptr }
    }

    /// Returns the number of available (free) slots.
    #[inline]
    pub fn available(&self) -> usize {
        // SAFETY: Reading a usize is atomic on supported platforms,
        // and single-threaded access is required anyway
        unsafe { *self.free_count.get() }
    }

    /// Returns the total capacity of the pool.
    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Returns a mutable reference to the object at the given index.
    ///
    /// This method is useful when you have stored the index (e.g., in a hash map)
    /// and need to access the object directly without a PoolPtr. This is common
    /// in order book implementations where order IDs map to pool indices.
    ///
    /// # Arguments
    ///
    /// * `index` - The index into the pool's storage array
    ///
    /// # Returns
    ///
    /// `Some(&mut T)` if the index is within bounds, `None` otherwise.
    ///
    /// # Safety
    ///
    /// - The index must refer to an allocated slot (not a free slot)
    /// - The slot must have been initialized (written to) before reading
    /// - No other references (shared or mutable) to the same slot must exist
    /// - This method uses interior mutability; single-threaded access is required
    ///
    /// # Note
    ///
    /// This method does NOT check whether the slot is currently allocated.
    /// The caller must ensure the index refers to a valid, allocated slot.
    /// Using an index for a free (deallocated) slot is undefined behavior.
    #[inline]
    pub fn get_by_index(&self, index: usize) -> Option<&mut T> {
        if index >= N {
            return None;
        }

        // SAFETY: Caller guarantees the slot at index is allocated, initialized,
        // and no other references exist. Interior mutability is used intentionally
        // for single-threaded performance.
        unsafe {
            let storage = &mut *self.storage.get();
            Some(&mut *storage[index].as_mut_ptr())
        }
    }

    /// Returns a mutable reference to the object at the given index without bounds checking.
    ///
    /// This is the unchecked version of `get_by_index` for maximum performance
    /// in hot paths where the index is known to be valid.
    ///
    /// # Safety
    ///
    /// - The index must be less than N (the pool capacity)
    /// - The index must refer to an allocated slot (not a free slot)
    /// - The slot must have been initialized (written to) before reading
    /// - No other references (shared or mutable) to the same slot must exist
    /// - Single-threaded access is required (interior mutability)
    ///
    /// Violating any of these conditions results in undefined behavior.
    #[inline]
    pub unsafe fn get_by_index_unchecked(&self, index: usize) -> &mut T {
        debug_assert!(index < N, "index out of bounds");

        // SAFETY: Caller guarantees all safety requirements are met
        let storage = &mut *self.storage.get();
        &mut *storage[index].as_mut_ptr()
    }
}

impl<T, const N: usize> Default for MemPool<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: MemPool can be sent between threads, but should only be used
// from one thread at a time
unsafe impl<T: Send, const N: usize> Send for MemPool<T, N> {}

// Note: We intentionally do NOT implement Sync, as concurrent access is unsafe

impl<T> PoolPtr<T> {
    /// Returns the index of this slot in the pool.
    ///
    /// Useful for debugging or logging purposes.
    #[inline]
    pub fn index(&self) -> usize {
        self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocation_and_deallocation() {
        let pool: MemPool<u64, 4> = MemPool::new();

        assert_eq!(pool.capacity(), 4);
        assert_eq!(pool.available(), 4);

        // Allocate a slot
        let ptr = pool.allocate().expect("should allocate");
        assert_eq!(pool.available(), 3);

        // Write and read
        *pool.get_mut(&ptr) = 42;
        assert_eq!(*pool.get(&ptr), 42);

        // Deallocate
        pool.deallocate(ptr);
        assert_eq!(pool.available(), 4);
    }

    #[test]
    fn test_pool_exhaustion() {
        let pool: MemPool<u32, 2> = MemPool::new();

        // Allocate all slots
        let ptr1 = pool.allocate().expect("first allocation");
        let ptr2 = pool.allocate().expect("second allocation");

        assert_eq!(pool.available(), 0);

        // Pool should be exhausted
        assert!(pool.allocate().is_none());
        assert!(pool.allocate().is_none());

        // Clean up
        pool.deallocate(ptr1);
        pool.deallocate(ptr2);
    }

    #[test]
    fn test_reuse_of_deallocated_slots() {
        let pool: MemPool<i32, 2> = MemPool::new();

        // Allocate both slots
        let ptr1 = pool.allocate().expect("first allocation");
        let ptr2 = pool.allocate().expect("second allocation");

        *pool.get_mut(&ptr1) = 100;
        *pool.get_mut(&ptr2) = 200;

        let idx1 = ptr1.index();

        // Deallocate first slot
        pool.deallocate(ptr1);
        assert_eq!(pool.available(), 1);

        // Reallocate - should get the same slot back (LIFO)
        let ptr3 = pool.allocate().expect("reallocation");
        assert_eq!(ptr3.index(), idx1);
        assert_eq!(pool.available(), 0);

        // Write new value
        *pool.get_mut(&ptr3) = 300;
        assert_eq!(*pool.get(&ptr3), 300);

        // Clean up
        pool.deallocate(ptr2);
        pool.deallocate(ptr3);
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn test_multiple_allocation_deallocation_cycles() {
        let pool: MemPool<String, 3> = MemPool::new();

        // First cycle - use ptr::write for uninitialized memory
        let ptr1 = pool.allocate().unwrap();
        unsafe { std::ptr::write(pool.get_mut(&ptr1), String::from("hello")) };
        assert_eq!(pool.get(&ptr1), "hello");

        // Drop the string before deallocating
        unsafe { std::ptr::drop_in_place(pool.get_mut(&ptr1)) };
        pool.deallocate(ptr1);

        // Second cycle - memory is now uninitialized again, use ptr::write
        let ptr2 = pool.allocate().unwrap();
        unsafe { std::ptr::write(pool.get_mut(&ptr2), String::from("world")) };
        assert_eq!(pool.get(&ptr2), "world");

        // Clean up
        unsafe { std::ptr::drop_in_place(pool.get_mut(&ptr2)) };
        pool.deallocate(ptr2);
    }

    #[test]
    fn test_full_capacity_usage() {
        const SIZE: usize = 64;
        let pool: MemPool<usize, SIZE> = MemPool::new();

        // Allocate all slots and store pointers
        let mut ptrs = Vec::with_capacity(SIZE);
        for i in 0..SIZE {
            let ptr = pool.allocate().expect("should allocate");
            *pool.get_mut(&ptr) = i;
            ptrs.push(ptr);
        }

        assert_eq!(pool.available(), 0);
        assert!(pool.allocate().is_none());

        // Verify all values
        for (i, ptr) in ptrs.iter().enumerate() {
            assert_eq!(*pool.get(ptr), i);
        }

        // Deallocate all
        for ptr in ptrs {
            pool.deallocate(ptr);
        }

        assert_eq!(pool.available(), SIZE);
    }

    #[test]
    fn test_with_complex_type() {
        #[derive(Debug, PartialEq)]
        struct Order {
            id: u64,
            price: f64,
            quantity: u32,
        }

        let pool: MemPool<Order, 8> = MemPool::new();

        let ptr = pool.allocate().unwrap();
        *pool.get_mut(&ptr) = Order {
            id: 12345,
            price: 99.99,
            quantity: 100,
        };

        let order = pool.get(&ptr);
        assert_eq!(order.id, 12345);
        assert_eq!(order.price, 99.99);
        assert_eq!(order.quantity, 100);

        pool.deallocate(ptr);
    }

    #[test]
    fn test_interleaved_operations() {
        let pool: MemPool<u8, 4> = MemPool::new();

        let a = pool.allocate().unwrap();
        let b = pool.allocate().unwrap();

        *pool.get_mut(&a) = 1;
        *pool.get_mut(&b) = 2;

        pool.deallocate(a);

        let c = pool.allocate().unwrap();
        *pool.get_mut(&c) = 3;

        let d = pool.allocate().unwrap();
        *pool.get_mut(&d) = 4;

        assert_eq!(*pool.get(&b), 2);
        assert_eq!(*pool.get(&c), 3);
        assert_eq!(*pool.get(&d), 4);

        pool.deallocate(b);
        pool.deallocate(c);
        pool.deallocate(d);

        assert_eq!(pool.available(), 4);
    }

    #[test]
    #[should_panic(expected = "capacity must be greater than 0")]
    fn test_zero_capacity_panics() {
        let _pool: MemPool<u8, 0> = MemPool::new();
    }

    #[test]
    fn test_get_by_index() {
        let pool: MemPool<u64, 4> = MemPool::new();

        // Allocate a slot and remember the index
        let ptr = pool.allocate().expect("should allocate");
        let index = ptr.index();

        // Write via the normal method
        *pool.get_mut(&ptr) = 42;

        // Access by index
        let value = pool.get_by_index(index).expect("should get by index");
        assert_eq!(*value, 42);

        // Modify by index
        *value = 100;

        // Verify the change is visible via PoolPtr
        assert_eq!(*pool.get(&ptr), 100);

        // Out of bounds should return None
        assert!(pool.get_by_index(100).is_none());

        pool.deallocate(ptr);
    }

    #[test]
    fn test_get_by_index_unchecked() {
        let pool: MemPool<u64, 4> = MemPool::new();

        // Allocate multiple slots
        let ptr1 = pool.allocate().expect("should allocate");
        let ptr2 = pool.allocate().expect("should allocate");
        let idx1 = ptr1.index();
        let idx2 = ptr2.index();

        *pool.get_mut(&ptr1) = 111;
        *pool.get_mut(&ptr2) = 222;

        // Access via unchecked method
        unsafe {
            assert_eq!(*pool.get_by_index_unchecked(idx1), 111);
            assert_eq!(*pool.get_by_index_unchecked(idx2), 222);

            // Modify via unchecked method
            *pool.get_by_index_unchecked(idx1) = 333;
        }

        // Verify the change
        assert_eq!(*pool.get(&ptr1), 333);

        pool.deallocate(ptr1);
        pool.deallocate(ptr2);
    }
}
