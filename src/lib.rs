//! CLIProxyAPI plugin that fills missing model details from models.dev.
//!
//! CPA loads this library as a native plugin and talks to it through the C
//! functions in `abi`. Everything else is plain Rust.

mod abi;
mod cache;
mod catalog;
mod error;
mod fill;
mod host;
mod loopback;
mod management;
mod plugin;
