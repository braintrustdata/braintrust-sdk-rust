use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_QUEUE_MAX_SIZE: usize = 15000;
const DEFAULT_QUEUE_DROP_LOGGING_PERIOD_SECS: u64 = 60;
const DEFAULT_BATCH_MAX_ITEMS: usize = 100;
pub(crate) const DEFAULT_BATCH_MAX_BYTES: usize = 3 * 1024 * 1024; // 3MB (half of BRAINTRUST_MAX_REQUEST_SIZE)

// 2 retries = 3 total attempts, matching the TypeScript SDK's default numTries = 3.
// When BRAINTRUST_NUM_RETRIES=N is set, both SDKs produce N+1 total attempts
// (TypeScript does `numTries = env + 1`; Rust loops `0..=num_retries`).
const DEFAULT_NUM_RETRIES: usize = 2;
const DEFAULT_FLUSH_CHUNK_SIZE: usize = 25;

fn default_queue_max_size() -> usize {
    std::env::var("BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_QUEUE_MAX_SIZE)
}

fn default_queue_drop_logging_period() -> Duration {
    Duration::from_secs(
        std::env::var("BRAINTRUST_QUEUE_DROP_LOGGING_PERIOD")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_QUEUE_DROP_LOGGING_PERIOD_SECS),
    )
}

fn default_batch_max_items() -> usize {
    std::env::var("BRAINTRUST_DEFAULT_BATCH_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_BATCH_MAX_ITEMS)
}

fn default_batch_max_bytes() -> usize {
    std::env::var("BRAINTRUST_MAX_REQUEST_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|size| size / 2)
        .unwrap_or(DEFAULT_BATCH_MAX_BYTES)
}

fn default_num_retries() -> usize {
    std::env::var("BRAINTRUST_NUM_RETRIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_NUM_RETRIES)
}

fn default_flush_chunk_size() -> usize {
    if let Ok(s) = std::env::var("BRAINTRUST_LOG_FLUSH_CHUNK_SIZE") {
        if let Ok(v) = s.parse::<usize>() {
            if v > 0 {
                return v;
            }
            tracing::warn!(
                value = v,
                "BRAINTRUST_LOG_FLUSH_CHUNK_SIZE must be > 0, using default ({})",
                DEFAULT_FLUSH_CHUNK_SIZE
            );
        }
    }
    DEFAULT_FLUSH_CHUNK_SIZE
}

fn default_sync_flush() -> bool {
    matches!(
        std::env::var("BRAINTRUST_SYNC_FLUSH").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn default_failed_publish_payloads_dir() -> Option<PathBuf> {
    std::env::var("BRAINTRUST_FAILED_PUBLISH_PAYLOADS_DIR")
        .ok()
        .map(PathBuf::from)
}

fn default_all_publish_payloads_dir() -> Option<PathBuf> {
    std::env::var("BRAINTRUST_ALL_PUBLISH_PAYLOADS_DIR")
        .ok()
        .map(PathBuf::from)
}

/// Configuration for the log queue and pipeline workers.
///
/// Defaults are loaded from environment variables, matching the TypeScript SDK's
/// behavior. Use the builder to override individual fields:
///
/// ```
/// use braintrust_sdk_rust::LogQueueConfig;
///
/// let config = LogQueueConfig::builder()
///     .batch_max_items(50)
///     .num_retries(5)
///     .build();
/// ```
#[derive(bon::Builder, Debug, Clone)]
pub struct LogQueueConfig {
    /// Max queue size (BRAINTRUST_QUEUE_DROP_EXCEEDING_MAXSIZE, default: 15000)
    #[builder(default = default_queue_max_size())]
    queue_max_size: usize,

    /// Drop log throttle period (BRAINTRUST_QUEUE_DROP_LOGGING_PERIOD, default: 60s)
    #[builder(default = default_queue_drop_logging_period())]
    queue_drop_logging_period: Duration,

    /// Max items per batch (BRAINTRUST_DEFAULT_BATCH_SIZE, default: 100)
    #[builder(default = default_batch_max_items())]
    batch_max_items: usize,

    /// Max bytes per batch (BRAINTRUST_MAX_REQUEST_SIZE / 2, default: 3MB)
    #[builder(default = default_batch_max_bytes())]
    batch_max_bytes: usize,

    /// Retry count (BRAINTRUST_NUM_RETRIES, default: 2 retries = 3 total attempts).
    /// When the env var is set to N, both this SDK and the TypeScript SDK produce N+1 total
    /// attempts (TypeScript does `numTries = env + 1`; Rust loops `0..=num_retries`).
    #[builder(default = default_num_retries())]
    num_retries: usize,

    /// Sync flush mode (BRAINTRUST_SYNC_FLUSH, default: false)
    #[builder(default = default_sync_flush())]
    sync_flush: bool,

    /// Chunk size for flush processing (BRAINTRUST_LOG_FLUSH_CHUNK_SIZE, default: 25).
    /// Rows drained from the queue are merged and sent in sequential chunks of this size.
    /// Each chunk's batches are sent concurrently (matching TypeScript SDK behavior).
    #[builder(default = default_flush_chunk_size())]
    flush_chunk_size: usize,

    /// Debug: save failed payloads (BRAINTRUST_FAILED_PUBLISH_PAYLOADS_DIR)
    failed_publish_payloads_dir: Option<PathBuf>,

    /// Debug: save all payloads (BRAINTRUST_ALL_PUBLISH_PAYLOADS_DIR)
    all_publish_payloads_dir: Option<PathBuf>,

    /// When true, the queue drops new items once it reaches `queue_max_size`.
    /// Defaults to false (unbounded), matching the TypeScript SDK's default behavior where
    /// data loss via queue overflow is opt-in. Set to true to apply a hard cap and protect
    /// against runaway memory growth in high-throughput scenarios.
    #[builder(default = false)]
    enforce_queue_size_limit: bool,
}

impl Default for LogQueueConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl LogQueueConfig {
    pub fn queue_max_size(&self) -> usize {
        self.queue_max_size
    }

    pub fn queue_drop_logging_period(&self) -> Duration {
        self.queue_drop_logging_period
    }

    pub fn batch_max_items(&self) -> usize {
        self.batch_max_items
    }

    pub fn batch_max_bytes(&self) -> usize {
        self.batch_max_bytes
    }

    pub fn num_retries(&self) -> usize {
        self.num_retries
    }

    pub fn sync_flush(&self) -> bool {
        self.sync_flush
    }

    pub fn flush_chunk_size(&self) -> usize {
        self.flush_chunk_size
    }

    pub fn failed_publish_payloads_dir(&self) -> Option<PathBuf> {
        self.failed_publish_payloads_dir
            .clone()
            .or_else(default_failed_publish_payloads_dir)
    }

    pub fn all_publish_payloads_dir(&self) -> Option<PathBuf> {
        self.all_publish_payloads_dir
            .clone()
            .or_else(default_all_publish_payloads_dir)
    }

    pub fn enforce_queue_size_limit(&self) -> bool {
        self.enforce_queue_size_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = LogQueueConfig::default();
        assert_eq!(config.queue_max_size(), DEFAULT_QUEUE_MAX_SIZE);
        assert_eq!(config.batch_max_items(), DEFAULT_BATCH_MAX_ITEMS);
        assert_eq!(config.batch_max_bytes(), DEFAULT_BATCH_MAX_BYTES);
        assert_eq!(config.num_retries(), DEFAULT_NUM_RETRIES);
        assert!(!config.sync_flush());
        assert!(!config.enforce_queue_size_limit()); // unbounded by default (matches TS SDK)
    }

    #[test]
    fn test_enforce_queue_size_limit_can_be_enabled() {
        let config = LogQueueConfig::builder()
            .enforce_queue_size_limit(true)
            .build();
        assert!(config.enforce_queue_size_limit());
    }

    #[test]
    fn test_config_builder() {
        let config = LogQueueConfig::builder()
            .queue_max_size(5000)
            .batch_max_items(50)
            .num_retries(5)
            .sync_flush(true)
            .build();

        assert_eq!(config.queue_max_size(), 5000);
        assert_eq!(config.batch_max_items(), 50);
        assert_eq!(config.num_retries(), 5);
        assert!(config.sync_flush());
    }

    #[test]
    fn test_sync_flush_env_var_accepts_truthy_strings() {
        for truthy in &["1", "true", "yes"] {
            unsafe { std::env::set_var("BRAINTRUST_SYNC_FLUSH", truthy) };
            assert!(
                default_sync_flush(),
                "BRAINTRUST_SYNC_FLUSH={truthy} should enable sync flush"
            );
        }
        for falsy in &["0", "false", "no", ""] {
            unsafe { std::env::set_var("BRAINTRUST_SYNC_FLUSH", falsy) };
            assert!(
                !default_sync_flush(),
                "BRAINTRUST_SYNC_FLUSH={falsy} should not enable sync flush"
            );
        }
        unsafe { std::env::remove_var("BRAINTRUST_SYNC_FLUSH") };
        assert!(!default_sync_flush(), "unset should not enable sync flush");
    }

    #[test]
    fn test_dir_getter_returns_none_when_unset() {
        let config = LogQueueConfig::builder().build();
        // Assumes env vars are not set in the test environment
        if std::env::var("BRAINTRUST_FAILED_PUBLISH_PAYLOADS_DIR").is_err() {
            assert_eq!(config.failed_publish_payloads_dir(), None);
        }
        if std::env::var("BRAINTRUST_ALL_PUBLISH_PAYLOADS_DIR").is_err() {
            assert_eq!(config.all_publish_payloads_dir(), None);
        }
    }

    #[test]
    fn test_dir_getter_returns_explicit_value() {
        let config = LogQueueConfig::builder()
            .failed_publish_payloads_dir(PathBuf::from("/tmp/failed"))
            .all_publish_payloads_dir(PathBuf::from("/tmp/all"))
            .build();

        assert_eq!(
            config.failed_publish_payloads_dir(),
            Some(PathBuf::from("/tmp/failed"))
        );
        assert_eq!(
            config.all_publish_payloads_dir(),
            Some(PathBuf::from("/tmp/all"))
        );
    }

    #[test]
    fn test_dir_getter_falls_back_to_env_var() {
        let failed_key = "BRAINTRUST_FAILED_PUBLISH_PAYLOADS_DIR";
        let all_key = "BRAINTRUST_ALL_PUBLISH_PAYLOADS_DIR";

        // Save previous values and set test values
        let prev_failed = std::env::var(failed_key).ok();
        let prev_all = std::env::var(all_key).ok();
        unsafe {
            std::env::set_var(failed_key, "/tmp/failed-env");
            std::env::set_var(all_key, "/tmp/all-env");
        }

        let config = LogQueueConfig::builder().build();
        let failed = config.failed_publish_payloads_dir();
        let all = config.all_publish_payloads_dir();

        // Restore env vars before asserting so they're cleaned up even on panic
        unsafe {
            match prev_failed {
                Some(v) => std::env::set_var(failed_key, v),
                None => std::env::remove_var(failed_key),
            }
            match prev_all {
                Some(v) => std::env::set_var(all_key, v),
                None => std::env::remove_var(all_key),
            }
        }

        assert_eq!(failed, Some(PathBuf::from("/tmp/failed-env")));
        assert_eq!(all, Some(PathBuf::from("/tmp/all-env")));
    }
}
