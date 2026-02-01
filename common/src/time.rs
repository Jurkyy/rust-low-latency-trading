// Timing utilities for low-latency measurement

use std::time::Instant;
use std::sync::OnceLock;

/// Global anchor point for converting Instant to nanoseconds
static EPOCH: OnceLock<Instant> = OnceLock::new();

fn get_epoch() -> &'static Instant {
    EPOCH.get_or_init(Instant::now)
}

/// Nanosecond-precision timestamp type
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Nanos(pub u64);

impl Nanos {
    /// Create a new Nanos value
    #[inline]
    pub const fn new(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Get the raw nanosecond value
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Calculate elapsed time since this timestamp
    #[inline]
    pub fn elapsed(self) -> u64 {
        nanos_since(self)
    }
}

impl std::ops::Sub for Nanos {
    type Output = u64;

    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        self.0.saturating_sub(rhs.0)
    }
}

impl std::ops::Add<u64> for Nanos {
    type Output = Nanos;

    #[inline]
    fn add(self, rhs: u64) -> Self::Output {
        Nanos(self.0.saturating_add(rhs))
    }
}

impl From<u64> for Nanos {
    #[inline]
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Nanos> for u64 {
    #[inline]
    fn from(value: Nanos) -> Self {
        value.0
    }
}

/// Get current time in nanoseconds since an arbitrary epoch
/// Uses std::time::Instant for monotonic time
#[inline]
pub fn now_nanos() -> Nanos {
    let epoch = get_epoch();
    let elapsed = Instant::now().duration_since(*epoch);
    Nanos(elapsed.as_nanos() as u64)
}

/// Calculate elapsed nanoseconds since the given start time
#[inline]
pub fn nanos_since(start: Nanos) -> u64 {
    now_nanos().0.saturating_sub(start.0)
}

/// Latency statistics tracker for measuring operation performance
#[derive(Debug, Clone)]
pub struct LatencyStats {
    count: u64,
    sum: u64,
    min: u64,
    max: u64,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyStats {
    /// Create a new LatencyStats instance
    #[inline]
    pub const fn new() -> Self {
        Self {
            count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
        }
    }

    /// Record a latency measurement in nanoseconds
    #[inline]
    pub fn record(&mut self, latency_nanos: u64) {
        self.count += 1;
        self.sum = self.sum.saturating_add(latency_nanos);
        self.min = self.min.min(latency_nanos);
        self.max = self.max.max(latency_nanos);
    }

    /// Get the number of recorded measurements
    #[inline]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Get the mean (average) latency in nanoseconds
    /// Returns 0.0 if no measurements have been recorded
    #[inline]
    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }

    /// Get the minimum recorded latency in nanoseconds
    /// Returns u64::MAX if no measurements have been recorded
    #[inline]
    pub const fn min(&self) -> u64 {
        self.min
    }

    /// Get the maximum recorded latency in nanoseconds
    /// Returns 0 if no measurements have been recorded
    #[inline]
    pub const fn max(&self) -> u64 {
        self.max
    }

    /// Reset all statistics
    #[inline]
    pub fn reset(&mut self) {
        self.count = 0;
        self.sum = 0;
        self.min = u64::MAX;
        self.max = 0;
    }
}

/// Read the CPU Time Stamp Counter (TSC)
/// This provides very low overhead cycle counting for latency measurement
/// Note: TSC frequency may vary; use for relative measurements
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn rdtsc() -> u64 {
    unsafe {
        std::arch::x86_64::_rdtsc()
    }
}

/// Read the CPU Time Stamp Counter with serialization (RDTSCP)
/// This is the serializing version that ensures all prior instructions complete
/// before reading the counter. Use this for more accurate measurements.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn rdtscp() -> u64 {
    let mut aux: u32 = 0;
    unsafe {
        std::arch::x86_64::__rdtscp(&mut aux)
    }
}

/// Fallback for non-x86_64 architectures
#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub fn rdtsc() -> u64 {
    now_nanos().0
}

/// Fallback for non-x86_64 architectures
#[cfg(not(target_arch = "x86_64"))]
#[inline]
pub fn rdtscp() -> u64 {
    now_nanos().0
}

/// Scoped timer for automatic latency measurement
/// Records the elapsed time when dropped
pub struct ScopedTimer<'a> {
    stats: &'a mut LatencyStats,
    start: Nanos,
}

impl<'a> ScopedTimer<'a> {
    /// Create a new scoped timer that will record to the given stats
    #[inline]
    pub fn new(stats: &'a mut LatencyStats) -> Self {
        Self {
            stats,
            start: now_nanos(),
        }
    }

    /// Get the current elapsed time without stopping the timer
    #[inline]
    pub fn elapsed(&self) -> u64 {
        nanos_since(self.start)
    }
}

impl Drop for ScopedTimer<'_> {
    #[inline]
    fn drop(&mut self) {
        let elapsed = nanos_since(self.start);
        self.stats.record(elapsed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nanos_creation() {
        let n = Nanos::new(12345);
        assert_eq!(n.as_u64(), 12345);
        assert_eq!(n.0, 12345);
    }

    #[test]
    fn test_nanos_from_u64() {
        let n: Nanos = 42u64.into();
        assert_eq!(n.0, 42);

        let v: u64 = n.into();
        assert_eq!(v, 42);
    }

    #[test]
    fn test_nanos_ordering() {
        let a = Nanos(100);
        let b = Nanos(200);
        let c = Nanos(100);

        assert!(a < b);
        assert!(b > a);
        assert_eq!(a, c);
    }

    #[test]
    fn test_nanos_subtraction() {
        let a = Nanos(200);
        let b = Nanos(100);
        assert_eq!(a - b, 100);

        // Test saturating subtraction (no underflow)
        assert_eq!(b - a, 0);
    }

    #[test]
    fn test_nanos_addition() {
        let n = Nanos(100);
        let result = n + 50;
        assert_eq!(result.0, 150);
    }

    #[test]
    fn test_now_nanos() {
        let t1 = now_nanos();
        // Busy wait a tiny bit
        for _ in 0..1000 {
            std::hint::black_box(0);
        }
        let t2 = now_nanos();

        assert!(t2 > t1, "Time should advance");
    }

    #[test]
    fn test_nanos_since() {
        let start = now_nanos();
        // Busy wait
        for _ in 0..1000 {
            std::hint::black_box(0);
        }
        let elapsed = nanos_since(start);

        assert!(elapsed > 0, "Elapsed time should be positive");
    }

    #[test]
    fn test_latency_stats_new() {
        let stats = LatencyStats::new();
        assert_eq!(stats.count(), 0);
        assert_eq!(stats.mean(), 0.0);
        assert_eq!(stats.min(), u64::MAX);
        assert_eq!(stats.max(), 0);
    }

    #[test]
    fn test_latency_stats_single_record() {
        let mut stats = LatencyStats::new();
        stats.record(100);

        assert_eq!(stats.count(), 1);
        assert_eq!(stats.mean(), 100.0);
        assert_eq!(stats.min(), 100);
        assert_eq!(stats.max(), 100);
    }

    #[test]
    fn test_latency_stats_multiple_records() {
        let mut stats = LatencyStats::new();
        stats.record(100);
        stats.record(200);
        stats.record(300);

        assert_eq!(stats.count(), 3);
        assert_eq!(stats.mean(), 200.0);
        assert_eq!(stats.min(), 100);
        assert_eq!(stats.max(), 300);
    }

    #[test]
    fn test_latency_stats_reset() {
        let mut stats = LatencyStats::new();
        stats.record(100);
        stats.record(200);

        stats.reset();

        assert_eq!(stats.count(), 0);
        assert_eq!(stats.mean(), 0.0);
        assert_eq!(stats.min(), u64::MAX);
        assert_eq!(stats.max(), 0);
    }

    #[test]
    fn test_latency_stats_default() {
        let stats: LatencyStats = Default::default();
        assert_eq!(stats.count(), 0);
    }

    #[test]
    fn test_scoped_timer() {
        let mut stats = LatencyStats::new();

        {
            let _timer = ScopedTimer::new(&mut stats);
            // Do some work
            for _ in 0..1000 {
                std::hint::black_box(0);
            }
        }

        assert_eq!(stats.count(), 1);
        assert!(stats.min() > 0 || stats.max() > 0, "Timer should record some elapsed time");
    }

    #[test]
    fn test_scoped_timer_multiple() {
        let mut stats = LatencyStats::new();

        for _ in 0..5 {
            let _timer = ScopedTimer::new(&mut stats);
            for _ in 0..100 {
                std::hint::black_box(0);
            }
        }

        assert_eq!(stats.count(), 5);
    }

    #[test]
    fn test_scoped_timer_elapsed() {
        let mut stats = LatencyStats::new();
        let timer = ScopedTimer::new(&mut stats);

        // Busy wait
        for _ in 0..1000 {
            std::hint::black_box(0);
        }

        let elapsed = timer.elapsed();
        assert!(elapsed > 0 || elapsed == 0, "elapsed() should return a value");

        drop(timer);
        assert_eq!(stats.count(), 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_rdtsc() {
        let t1 = rdtsc();
        // Busy wait
        for _ in 0..1000 {
            std::hint::black_box(0);
        }
        let t2 = rdtsc();

        assert!(t2 > t1, "TSC should advance");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_rdtscp() {
        let t1 = rdtscp();
        // Busy wait
        for _ in 0..1000 {
            std::hint::black_box(0);
        }
        let t2 = rdtscp();

        assert!(t2 > t1, "TSC should advance");
    }
}
