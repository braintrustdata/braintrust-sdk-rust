//! Direct, one-attempt Braintrust HTTP endpoints.
//!
//! Calls submit immediately and return [`crate::Completion`] tokens. Dropping
//! a token leaves accepted work running. Configure an explicit runtime or use
//! the default owned runtime, then call [`Client::shutdown`] to close admission
//! and drain accepted requests under a deadline.
//!
//! Explicit credentials and routes are immutable. Each accepted endpoint call
//! makes one HTTP attempt: no implicit login, retries, batching, overflow upload
//! or resource cache. SDK resource policy reuses the private HTTP transport
//! inside its own admitted operations.
//!
//! ```no_run
//! use braintrust_sdk_rust::api::{Client, Config, logs3::InsertRows};
//! use serde_json::json;
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::new(Config::new("token"))?;
//! let completion = client.logs3().insert(InsertRows::new(vec![
//!     json!({"id": "row", "project_id": "project"}),
//! ])); // request admission happens now
//! completion.await?;
//! client.shutdown(Duration::from_secs(10)).await?;
//! # Ok(())
//! # }
//! ```

pub mod btql;
mod client;
mod config;
pub mod datasets;
mod error;
pub mod experiments;
pub mod login;
pub mod logs3;
pub mod projects;

pub use client::Client;
pub use config::{Config, DEFAULT_API_URL, DEFAULT_APP_URL};
pub use error::Error;
use transport::Plane;

mod transport;
pub(crate) use transport::Transport;
