[![Crates.io](https://img.shields.io/crates/v/mrpc.svg)](https://crates.io/crates/mrpc)
[![Documentation](https://docs.rs/mrpc/badge.svg)](https://docs.rs/mrpc)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

# mrpc

A MessagePack-RPC implementation in Rust.

## Features

- Write asynchronous RPC servers and clients
- Support for TCP and Unix domain sockets
- Full msgpack-rpc implementation (requests, responses, notifications)
- Support for bidirectional communication - both servers and clients can handle incoming RPC messages
- Built on `tokio` for async I/O
- Uses `rmpv` for MessagePack serialization
- Optional `serde` feature for typed calls and helpers


## Quick Start

<!-- snips: examples/echo.rs#content -->
```rust
use mrpc::{Client, Connection, Result, RpcSender, Server};
use rmpv::Value;

/// Echo service that returns the method and first parameter.
#[derive(Clone, Default)]
struct Echo;

#[async_trait::async_trait]
impl Connection for Echo {
    async fn handle_request(
        &self,
        _: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        Ok(format!("{} -> {}", method, params[0]).into())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Using the default constructor as our ConnectionMaker
    let server = Server::from_fn(Echo::default).tcp("127.0.0.1:0").await?;
    let addr = server.local_addr().unwrap();
    tokio::spawn(server.run());

    // `Connection` is implemented for (), as a convenience for clients who don't need to handle
    // requests or responses.
    let client = Client::connect_tcp(&addr.to_string(), ()).await?;
    let result = client
        .send_request("echo", &[Value::String("Hello there!".into())])
        .await?;
    println!("{}", result);
    Ok(())
}
```

## Serde Feature

Enable the `serde` feature to unlock typed `call`/`notify` APIs and helpers for converting between
Rust types and MessagePack values/params.

Typed client-side calls (params and responses):

<!-- snips: examples/serde_support.rs#client -->
```rust
client.notify("log", &"hello from serde").await?;
let sum: i64 = client.call("add", &(5_i64, 3_i64)).await?;
println!("{sum}");
```

Typed server-side helpers (decode params and encode responses):

<!-- snips: examples/serde_support.rs#server -->
```rust
/// Typed handler for a positional-params method.
fn add(params: Vec<Value>) -> Result<Value> {
    let (a, b): (i64, i64) = deserialize_params(params)?;
    serialize_value(&(a + b))
}

/// Typed handler for a single-param notification.
fn log(params: Vec<Value>) -> Result<()> {
    let message: String = deserialize_param(params)?;
    println!("{message}");
    Ok(())
}
```
