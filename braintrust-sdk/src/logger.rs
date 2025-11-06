//! Logger entry point for the Braintrust SDK

use crate::client::HttpClient;
use crate::error::{BraintrustError, Result};
use crate::queue::EventQueue;
use crate::span::{Span, SpanImpl};
use crate::types::StartSpanOptions;
use std::env;
use std::sync::Arc;

/// Default batch size for event queue
const DEFAULT_BATCH_SIZE: usize = 100;

/// Options for configuring a Logger
#[derive(Debug, Clone)]
pub struct LoggerOptions {
    /// Project name or ID
    pub project: String,

    /// API key (will be read from env if not provided)
    pub api_key: Option<String>,

    /// API URL (will be read from env if not provided)
    pub api_url: Option<String>,

    /// Batch size for event queue
    pub batch_size: Option<usize>,
}

impl LoggerOptions {
    /// Create new logger options with a project name
    pub fn new(project: impl Into<String>) -> Self {
        LoggerOptions {
            project: project.into(),
            api_key: None,
            api_url: None,
            batch_size: None,
        }
    }

    /// Set the API key
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Set the API URL
    pub fn with_api_url(mut self, api_url: impl Into<String>) -> Self {
        self.api_url = Some(api_url.into());
        self
    }

    /// Set the batch size
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = Some(batch_size);
        self
    }
}

/// Logger for creating and managing spans
pub struct Logger {
    project_id: String,
    client: Arc<HttpClient>,
    queue: Arc<EventQueue>,
    _background_task: Option<tokio::task::JoinHandle<()>>,
}

impl Logger {
    /// Create a new logger with the given options (async version)
    ///
    /// # Arguments
    ///
    /// * `options` - Configuration options for the logger
    ///
    /// # Returns
    ///
    /// A new logger instance
    ///
    /// # Errors
    ///
    /// Returns an error if the API key is not provided or invalid
    pub async fn new_async(options: LoggerOptions) -> Result<Self> {
        // Load .env file if present (for local development)
        let _ = dotenvy::dotenv();

        // Get API key from options or environment
        let api_key = options
            .api_key
            .or_else(|| env::var("BRAINTRUST_API_KEY").ok())
            .ok_or_else(|| {
                BraintrustError::ConfigError(
                    "API key not provided. Set BRAINTRUST_API_KEY environment variable or pass via options".to_string(),
                )
            })?;

        // Get API URL from options or environment
        let api_url = options
            .api_url
            .or_else(|| env::var("BRAINTRUST_API_URL").ok());

        // Create HTTP client
        let client = Arc::new(HttpClient::new(api_key, api_url)?);

        // Get organization ID
        let org_id = client.get_org_id().await?;

        // Register or get the project
        let project_id = client.register_project(options.project, org_id).await?;

        // Create event queue
        let batch_size = options.batch_size.unwrap_or(DEFAULT_BATCH_SIZE);
        let queue = Arc::new(EventQueue::new(batch_size));

        // Start background processor
        let background_task = queue.clone().start_background_processor(client.clone());

        Ok(Logger {
            project_id,
            client,
            queue,
            _background_task: Some(background_task),
        })
    }

    /// Create a new logger with the given options (blocking version)
    ///
    /// This is a convenience wrapper around `new_async` that creates a new Tokio runtime.
    /// Use `new_async` if you're already in an async context.
    ///
    /// # Arguments
    ///
    /// * `options` - Configuration options for the logger
    ///
    /// # Returns
    ///
    /// A new logger instance
    ///
    /// # Errors
    ///
    /// Returns an error if the API key is not provided or invalid
    pub fn new(options: LoggerOptions) -> Result<Self> {
        // Create a new runtime for the blocking call
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(Self::new_async(options))
    }

    /// Start a new span
    ///
    /// # Arguments
    ///
    /// * `options` - Optional configuration for the span
    ///
    /// # Returns
    ///
    /// A new span instance
    pub fn start_span(&self, options: Option<StartSpanOptions>) -> Arc<dyn Span> {
        let options = options.unwrap_or_default();
        SpanImpl::new(
            self.queue.clone(),
            options,
            Some(self.project_id.clone()),
        )
    }

    /// Flush all pending events to the server
    ///
    /// # Returns
    ///
    /// Result indicating success or failure
    pub async fn flush(&self) -> Result<()> {
        self.queue.flush(&self.client).await
    }

    /// Get the project ID
    pub fn project_id(&self) -> &str {
        &self.project_id
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        // Try to flush any pending events before dropping
        if !self.queue.is_empty() {
            eprintln!("🔄 Flushing {} pending events on logger drop...", self.queue.len());

            // Try to use the current runtime if available
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let queue = self.queue.clone();
                let client = self.client.clone();

                // Spawn a blocking task to flush synchronously
                let _ = std::thread::spawn(move || {
                    handle.block_on(async move {
                        match queue.flush(&client).await {
                            Ok(_) => eprintln!("✅ Events flushed successfully"),
                            Err(e) => eprintln!("⚠️  Failed to flush events: {}", e),
                        }
                    });
                }).join();
            }
        }

        // Abort the background task when the logger is dropped
        if let Some(task) = self._background_task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logger_options() {
        let options = LoggerOptions::new("test-project")
            .with_api_key("test-key")
            .with_api_url("https://test.example.com")
            .with_batch_size(50);

        assert_eq!(options.project, "test-project");
        assert_eq!(options.api_key, Some("test-key".to_string()));
        assert_eq!(options.api_url, Some("https://test.example.com".to_string()));
        assert_eq!(options.batch_size, Some(50));
    }

    // Note: Logger creation tests are skipped because they depend on environment setup
    // and may interfere with other tests. Use integration tests with memory logger instead.
}
