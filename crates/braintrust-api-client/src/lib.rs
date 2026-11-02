#![doc = include_str!("../README.md")]

pub mod backend;
mod client;
#[allow(dead_code, clippy::all, rustdoc::bare_urls)]
#[rustfmt::skip]
mod generated;
mod request;
mod response;
pub mod transport;

pub use client::{
    Client, ClientBuilder, ClientContext, ClientInitError, DEFAULT_API_URL, DEFAULT_APP_URL,
};
pub use generated::client::{HttpClient as GeneratedClient, *};
pub use generated::{sse, types};
pub(crate) use request::{Request, Transport};
pub use response::ResponseValue;
/// Errors from login, queries, logging and other SDK operations.
pub type Error<E = serde_json::Value> = ApiOpError<E>;
