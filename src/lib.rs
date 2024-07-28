//! MessagePack-RPC implementation in Rust.
//!
//! Provides asynchronous RPC servers and clients over TCP and Unix domain sockets.
//! Implements the full MessagePack-RPC specification: requests, responses, and notifications.
//!
//! Both servers and clients can handle incoming RPC messages, enabling bidirectional communication.
//!
//! To implement a server:
//! 1. Implement the `RpcService` trait
//! 2. Create a `Server` with the service
//! 3. Call `server.tcp(addr)` or `server.unix(path)`
//! 4. Call `server.run()`
//!
//! To implement a client:
//! 1. Optionally implement `RpcService` to handle incoming messages
//! 2. Create a `Client` via `Client::connect_tcp(addr)` or `Client::connect_unix(path)`
//! 3. Use `client.send_request()` or `client.send_notification()`
//!
//! Uses `tokio` for async I/O and `rmpv` for MessagePack serialization.

mod connection;
mod error;
mod message;
mod transport;

pub use connection::*;
pub use error::*;
pub use message::*;
pub use transport::*;

pub use rmpv::Value;
