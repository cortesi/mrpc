//! MessagePack-RPC implementation in Rust.
//!
//! Provides asynchronous RPC servers and clients over TCP and Unix domain sockets.
//! Implements the full MessagePack-RPC specification.
//!
//! Both servers and clients can handle incoming RPC messages, enabling bidirectional
//! communication.
//!
//! To implement a server:
//! 1. Implement the `Connection` trait
//! 2. Create a `Server` with the service
//! 3. Call `server.tcp(addr)` or `server.unix(path)`
//! 4. Call `server.run()`
//!
//! To implement a client:
//! 1. Implement the `Connection` trait
//! 2. Create a `Client` via `Client::connect_tcp(addr)` or `Client::connect_unix(path)`
//! 3. Use `client.send_request()` or `client.send_notification()`

mod connection;
/// Error types for RPC operations.
mod error;
mod message;
mod transport;

pub use connection::*;
pub use error::*;
pub use message::*;
pub use rmpv::Value;
pub use transport::*;
