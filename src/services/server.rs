//! HTTP API server using Rouille — a synchronous micro-web-framework.
//!
//! Rouille replaces the previous vendored tiny-http + matchit + hand-rolled Range.
//! Benefits:
//!   - `router!` macro for clean URL dispatch
//!   - Built-in thread pool via `Server::pool_size()`
//!   - Range support is implemented manually via `FileStream` + `ResponseBody::from_reader_and_size`

mod core;
mod handle;
mod utils;

pub use core::start_server;

pub struct ServerConfig {
    pub bind_addr: String,
    pub port: u16,
    pub max_connections: u32,
    pub auth_username: String,
    pub auth_password: String,
}
