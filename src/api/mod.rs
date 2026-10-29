//! Direct, one-attempt Braintrust HTTP endpoints.
//!
//! This is the transport foundation of the SDK refactor. It has no background
//! login, queue, retries, batching or SDK resource cache. Construct it with
//! explicit credentials/endpoints; a login response never changes its scope.
//! Endpoint methods are direct async calls in this incremental layer. The eager
//! completion facade is introduced separately, above the same endpoint logic.

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
use client::Plane;
pub use config::{Config, DEFAULT_API_URL, DEFAULT_APP_URL};
pub use error::Error;
