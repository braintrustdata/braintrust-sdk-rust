#![doc = include_str!("../README.md")]

// Progenitor preserves upstream bare URLs and emits unused helpers and style lints.
// Keep allowances scoped to generated code, including its generated doctests.
#[allow(dead_code, clippy::all, rustdoc::bare_urls)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/api.rs"));
}

mod client;
pub mod transport;

pub use client::{ClientBuilder, ClientContext, ClientInitError, DEFAULT_API_URL, DEFAULT_APP_URL};
pub use generated::*;
