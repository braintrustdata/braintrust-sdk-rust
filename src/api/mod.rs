//! Minimal typed access to Braintrust HTTP API endpoints.
//!
//! This module is intentionally a thin HTTP layer. It turns typed request
//! objects into query strings or JSON bodies, sends them to the corresponding
//! `/v1` endpoint, and deserializes the typed response. It does not retry,
//! batch, poll, cache, register resources, or interpret workflow semantics;
//! those behaviors belong in higher-level SDK abstractions.
//!
//! The API client is useful for read-oriented validation and for callers that
//! need direct access to backend resources:
//!
//! ```no_run
//! # fn main() {}
//! # #[cfg(feature = "internal-api")]
//! # async fn example() -> braintrust_sdk_rust::Result<()> {
//! use braintrust_sdk_rust::BraintrustClient;
//! use braintrust_sdk_rust::api::projects::ListProjectsRequest;
//!
//! let client = BraintrustClient::builder()
//!     .api_key("your-api-key")
//!     .blocking_login(true)
//!     .build()
//!     .await?;
//! let api = client.api().await?;
//! let mut request = ListProjectsRequest::default();
//! request.project_name = Some("Demo".to_string());
//! let projects = api.projects().list(request).await?;
//!
//! for project in projects.objects() {
//!     println!("{} {}", project.id(), project.name());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Response structs preserve unmodeled backend fields in an `extra()` map where
//! the endpoint can return additional properties.

#![cfg_attr(not(feature = "internal-api"), allow(dead_code, unused_imports))]

mod client;
pub(crate) mod config;
#[macro_use]
mod macros;
mod session;
mod shared;
#[cfg(test)]
mod test_support;
pub(crate) mod transport;

pub mod ai_secrets;
pub mod api_keys;
pub mod attachments;
pub mod brainstore_automation;
pub mod btql;
pub mod comparisons;
pub mod dataset_lookup;
pub mod datasets;
pub mod experiments;
pub mod function_types;
pub mod functions;
pub mod logs3;
pub mod project_automations;
pub mod project_logs;
pub mod projects;
pub mod prompts;
pub mod registrations;
pub mod summaries;

pub use client::ApiClient;
pub use config::{DEFAULT_API_URL, DEFAULT_APP_URL};
pub use session::{LoginState, OrgInfo};
pub use shared::FetchEventsRequest;
