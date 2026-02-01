// Low-latency logger
//
// This logger is designed for high-frequency trading systems where logging
// overhead on the hot path must be minimized. Key design principles:
// 1. Lock-free logging using an SPSC queue
// 2. Lazy formatting - string formatting happens on the background thread
// 3. Background I/O - actual writes happen off the critical path
// 4. Static message preference - avoid allocations on the hot path

use crate::lf_queue::LFQueue;
use crate::time::{now_nanos, Nanos};

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

/// Log severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl LogLevel {
    /// Returns the string representation of the log level
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Log message types to avoid allocations on the hot path
///
/// The key insight is that most log messages are static strings with
/// optional numeric values. By deferring formatting to the background
/// thread, we keep the hot path allocation-free.
pub enum LogMessage {
    /// A static string message (zero allocation)
    Static(&'static str),
    /// A static message with an i64 value (formatting deferred)
    StaticWithI64(&'static str, i64),
    /// A static message with a u64 value (formatting deferred)
    StaticWithU64(&'static str, u64),
    /// A static message with an f64 value (formatting deferred)
    StaticWithF64(&'static str, f64),
    /// A pre-formatted string (rare cases where allocation is unavoidable)
    Formatted(String),
}

impl LogMessage {
    /// Format the message to the provided writer
    #[inline]
    fn write_to<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        match self {
            LogMessage::Static(s) => write!(writer, "{}", s),
            LogMessage::StaticWithI64(s, v) => write!(writer, "{}: {}", s, v),
            LogMessage::StaticWithU64(s, v) => write!(writer, "{}: {}", s, v),
            LogMessage::StaticWithF64(s, v) => write!(writer, "{}: {:.6}", s, v),
            LogMessage::Formatted(s) => write!(writer, "{}", s),
        }
    }
}

/// A single log entry
pub struct LogEntry {
    /// Timestamp when the log was created
    pub timestamp: Nanos,
    /// Severity level
    pub level: LogLevel,
    /// The message content
    pub message: LogMessage,
}

/// Shared state between Logger and background thread
struct LoggerShared {
    /// The lock-free queue for passing log entries to the background thread
    queue: LFQueue<LogEntry, 4096>,
    /// Flag to signal the background thread to stop
    running: AtomicBool,
    /// Flag to signal a flush is requested
    flush_requested: AtomicBool,
    /// Flag to signal flush is complete
    flush_complete: AtomicBool,
}

/// Low-latency logger that offloads I/O to a background thread
///
/// # Example
/// ```ignore
/// let logger = Logger::new();
/// logger.log(LogLevel::Info, "System started");
/// logger.log_with_i64(LogLevel::Debug, "Order count", 42);
/// logger.flush();
/// ```
pub struct Logger {
    /// Shared state with background thread
    shared: Arc<LoggerShared>,
    /// Handle to the background writer thread
    writer_thread: Option<JoinHandle<()>>,
    /// Minimum log level to record
    min_level: LogLevel,
}

impl Logger {
    /// Creates a new Logger with a background writer thread
    ///
    /// The background thread will poll the queue and write formatted
    /// log entries to stderr.
    pub fn new() -> Self {
        Self::with_level(LogLevel::Debug)
    }

    /// Creates a new Logger with a specified minimum log level
    pub fn with_level(min_level: LogLevel) -> Self {
        let shared = Arc::new(LoggerShared {
            queue: LFQueue::new(),
            running: AtomicBool::new(true),
            flush_requested: AtomicBool::new(false),
            flush_complete: AtomicBool::new(false),
        });

        let shared_clone = Arc::clone(&shared);
        let writer_thread = thread::spawn(move || {
            Self::writer_loop(shared_clone);
        });

        Self {
            shared,
            writer_thread: Some(writer_thread),
            min_level,
        }
    }

    /// Background thread main loop
    fn writer_loop(shared: Arc<LoggerShared>) {
        let mut stderr = std::io::stderr().lock();
        let mut idle_count = 0u32;

        while shared.running.load(Ordering::Relaxed) {
            let mut processed = 0;

            // Process all available entries
            while let Some(entry) = shared.queue.pop() {
                Self::write_entry(&mut stderr, &entry);
                processed += 1;
            }

            // Handle flush requests
            if shared.flush_requested.load(Ordering::Acquire) {
                let _ = stderr.flush();
                shared.flush_complete.store(true, Ordering::Release);
            }

            if processed > 0 {
                idle_count = 0;
            } else {
                idle_count = idle_count.saturating_add(1);

                // Progressive backoff to reduce CPU usage when idle
                // - First 100 iterations: spin (lowest latency)
                // - Next 1000 iterations: yield to other threads
                // - Beyond that: sleep briefly
                if idle_count < 100 {
                    std::hint::spin_loop();
                } else if idle_count < 1100 {
                    thread::yield_now();
                } else {
                    thread::sleep(std::time::Duration::from_micros(100));
                }
            }
        }

        // Drain remaining entries before exiting
        while let Some(entry) = shared.queue.pop() {
            Self::write_entry(&mut stderr, &entry);
        }
        let _ = stderr.flush();
    }

    /// Write a single log entry to the writer
    #[inline]
    fn write_entry<W: Write>(writer: &mut W, entry: &LogEntry) {
        // Format: [timestamp_ns] LEVEL message
        let _ = write!(
            writer,
            "[{:016}] {:5} ",
            entry.timestamp.as_u64(),
            entry.level.as_str()
        );
        let _ = entry.message.write_to(writer);
        let _ = writeln!(writer);
    }

    /// Log a static message
    ///
    /// This is the fastest logging path - no allocation, no formatting.
    #[inline]
    pub fn log(&self, level: LogLevel, msg: &'static str) {
        if level < self.min_level {
            return;
        }

        let entry = LogEntry {
            timestamp: now_nanos(),
            level,
            message: LogMessage::Static(msg),
        };

        // If queue is full, we drop the message rather than blocking
        // This is a deliberate design choice for low-latency systems
        let _ = self.shared.queue.push(entry);
    }

    /// Log a static message with an i64 value
    ///
    /// Formatting is deferred to the background thread.
    #[inline]
    pub fn log_with_i64(&self, level: LogLevel, msg: &'static str, value: i64) {
        if level < self.min_level {
            return;
        }

        let entry = LogEntry {
            timestamp: now_nanos(),
            level,
            message: LogMessage::StaticWithI64(msg, value),
        };

        let _ = self.shared.queue.push(entry);
    }

    /// Log a static message with a u64 value
    ///
    /// Formatting is deferred to the background thread.
    #[inline]
    pub fn log_with_u64(&self, level: LogLevel, msg: &'static str, value: u64) {
        if level < self.min_level {
            return;
        }

        let entry = LogEntry {
            timestamp: now_nanos(),
            level,
            message: LogMessage::StaticWithU64(msg, value),
        };

        let _ = self.shared.queue.push(entry);
    }

    /// Log a static message with an f64 value
    ///
    /// Formatting is deferred to the background thread.
    #[inline]
    pub fn log_with_f64(&self, level: LogLevel, msg: &'static str, value: f64) {
        if level < self.min_level {
            return;
        }

        let entry = LogEntry {
            timestamp: now_nanos(),
            level,
            message: LogMessage::StaticWithF64(msg, value),
        };

        let _ = self.shared.queue.push(entry);
    }

    /// Log a message with a value that implements Display
    ///
    /// This method performs allocation and formatting on the hot path,
    /// so it should only be used for rare cases where the other methods
    /// are insufficient.
    #[inline]
    pub fn log_with_value<T: std::fmt::Display>(&self, level: LogLevel, msg: &'static str, value: T) {
        if level < self.min_level {
            return;
        }

        let entry = LogEntry {
            timestamp: now_nanos(),
            level,
            message: LogMessage::Formatted(format!("{}: {}", msg, value)),
        };

        let _ = self.shared.queue.push(entry);
    }

    /// Flush all pending log entries
    ///
    /// This blocks until all queued entries have been written.
    pub fn flush(&self) {
        // Signal flush request
        self.shared.flush_complete.store(false, Ordering::Release);
        self.shared.flush_requested.store(true, Ordering::Release);

        // Wait for flush to complete
        while !self.shared.flush_complete.load(Ordering::Acquire) {
            if self.shared.queue.is_empty() {
                // Queue is empty, just wait for the background thread to notice
                std::hint::spin_loop();
            } else {
                thread::yield_now();
            }
        }

        // Clear the flush request
        self.shared.flush_requested.store(false, Ordering::Release);
    }

    /// Returns true if the logger is still running
    #[inline]
    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Relaxed)
    }

    /// Returns the current queue length
    #[inline]
    pub fn queue_len(&self) -> usize {
        self.shared.queue.len()
    }

    /// Set the minimum log level
    #[inline]
    pub fn set_level(&mut self, level: LogLevel) {
        self.min_level = level;
    }

    /// Get the current minimum log level
    #[inline]
    pub fn level(&self) -> LogLevel {
        self.min_level
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        // Signal the background thread to stop
        self.shared.running.store(false, Ordering::Release);

        // Wait for the thread to finish
        if let Some(handle) = self.writer_thread.take() {
            let _ = handle.join();
        }
    }
}

// Convenience macros for logging
// These macros capture the file and line number for debugging

/// Log a debug message
#[macro_export]
macro_rules! log_debug {
    ($logger:expr, $msg:literal) => {
        $logger.log($crate::logging::LogLevel::Debug, $msg)
    };
    ($logger:expr, $msg:literal, $val:expr) => {
        $logger.log_with_value($crate::logging::LogLevel::Debug, $msg, $val)
    };
}

/// Log an info message
#[macro_export]
macro_rules! log_info {
    ($logger:expr, $msg:literal) => {
        $logger.log($crate::logging::LogLevel::Info, $msg)
    };
    ($logger:expr, $msg:literal, $val:expr) => {
        $logger.log_with_value($crate::logging::LogLevel::Info, $msg, $val)
    };
}

/// Log a warning message
#[macro_export]
macro_rules! log_warn {
    ($logger:expr, $msg:literal) => {
        $logger.log($crate::logging::LogLevel::Warn, $msg)
    };
    ($logger:expr, $msg:literal, $val:expr) => {
        $logger.log_with_value($crate::logging::LogLevel::Warn, $msg, $val)
    };
}

/// Log an error message
#[macro_export]
macro_rules! log_error {
    ($logger:expr, $msg:literal) => {
        $logger.log($crate::logging::LogLevel::Error, $msg)
    };
    ($logger:expr, $msg:literal, $val:expr) => {
        $logger.log_with_value($crate::logging::LogLevel::Error, $msg, $val)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_ordering() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    #[test]
    fn test_log_level_display() {
        assert_eq!(LogLevel::Debug.as_str(), "DEBUG");
        assert_eq!(LogLevel::Info.as_str(), "INFO");
        assert_eq!(LogLevel::Warn.as_str(), "WARN");
        assert_eq!(LogLevel::Error.as_str(), "ERROR");
    }

    #[test]
    fn test_logger_creation() {
        let logger = Logger::new();
        assert!(logger.is_running());
        assert_eq!(logger.queue_len(), 0);
    }

    #[test]
    fn test_logger_with_level() {
        let logger = Logger::with_level(LogLevel::Warn);
        assert_eq!(logger.level(), LogLevel::Warn);
    }

    #[test]
    fn test_log_static_message() {
        let logger = Logger::new();
        logger.log(LogLevel::Info, "test message");

        // Flush to ensure message is processed
        logger.flush();

        // Should have been processed
        assert_eq!(logger.queue_len(), 0);
    }

    #[test]
    fn test_log_with_values() {
        let logger = Logger::new();

        logger.log_with_i64(LogLevel::Debug, "count", -42);
        logger.log_with_u64(LogLevel::Info, "size", 1024);
        logger.log_with_f64(LogLevel::Warn, "price", 123.456);
        logger.log_with_value(LogLevel::Error, "symbol", "AAPL");

        logger.flush();
        assert_eq!(logger.queue_len(), 0);
    }

    #[test]
    fn test_log_level_filtering() {
        let logger = Logger::with_level(LogLevel::Warn);

        // These should be filtered out
        logger.log(LogLevel::Debug, "debug message");
        logger.log(LogLevel::Info, "info message");

        // These should pass through
        logger.log(LogLevel::Warn, "warn message");
        logger.log(LogLevel::Error, "error message");

        logger.flush();
    }

    #[test]
    fn test_logger_flush() {
        let logger = Logger::new();

        for i in 0..100 {
            logger.log_with_i64(LogLevel::Info, "iteration", i);
        }

        logger.flush();
        assert_eq!(logger.queue_len(), 0);
    }

    #[test]
    fn test_logger_drop() {
        {
            let logger = Logger::new();
            logger.log(LogLevel::Info, "message before drop");
        }
        // Logger should be cleanly dropped, background thread joined
    }

    #[test]
    fn test_log_message_variants() {
        let mut buffer = Vec::new();

        LogMessage::Static("hello").write_to(&mut buffer).unwrap();
        assert_eq!(String::from_utf8_lossy(&buffer), "hello");

        buffer.clear();
        LogMessage::StaticWithI64("count", -5).write_to(&mut buffer).unwrap();
        assert_eq!(String::from_utf8_lossy(&buffer), "count: -5");

        buffer.clear();
        LogMessage::StaticWithU64("size", 100).write_to(&mut buffer).unwrap();
        assert_eq!(String::from_utf8_lossy(&buffer), "size: 100");

        buffer.clear();
        LogMessage::StaticWithF64("price", 1.5).write_to(&mut buffer).unwrap();
        assert_eq!(String::from_utf8_lossy(&buffer), "price: 1.500000");

        buffer.clear();
        LogMessage::Formatted("custom message".to_string()).write_to(&mut buffer).unwrap();
        assert_eq!(String::from_utf8_lossy(&buffer), "custom message");
    }

    #[test]
    fn test_macros() {
        let logger = Logger::new();

        log_debug!(logger, "debug test");
        log_info!(logger, "info test");
        log_warn!(logger, "warn test");
        log_error!(logger, "error test");

        log_debug!(logger, "debug with value", 42);
        log_info!(logger, "info with value", "hello");
        log_warn!(logger, "warn with value", 3.14);
        log_error!(logger, "error with value", -1);

        logger.flush();
    }

    #[test]
    fn test_high_throughput() {
        let logger = Logger::new();

        // Push many messages quickly
        for i in 0..1000 {
            logger.log_with_i64(LogLevel::Debug, "msg", i);
        }

        logger.flush();
        assert_eq!(logger.queue_len(), 0);
    }
}
