[![Crates.io](https://img.shields.io/crates/v/mrpc.svg)](https://crates.io/crates/mrpc)
[![Documentation](https://docs.rs/mrpc/badge.svg)](https://docs.rs/mrpc)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

# mrpc

A MessagePack-RPC implementation in Rust.

## Features

- Asynchronous RPC servers and clients
- Support for TCP and Unix domain sockets
- Full msgpack-rpc implementation (requests, responses, notifications)
- Support for bidirectional communication - both servers and clients can handle incoming RPC messages
- Built on `tokio` for async I/O
- Uses `rmpv` for MessagePack serialization

## Quick Start

```rust
use mrpc::{Client, Result, RpcHandle, RpcSender, RpcService, Server};
use rmpv::Value;

#[derive(Clone)]
struct EchoService;

#[async_trait::async_trait]
impl RpcService for EchoService {
    async fn handle_request<S>(
        &self,
        _: RpcHandle,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        match method {
            "echo" => Ok(params[0].clone()),
            _ => Err(mrpc::RpcError::Protocol(format!(
                "Unknown method: {}",
                method
            ))),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let server = Server::new(EchoService).tcp("127.0.0.1:8080").await?;
    tokio::spawn(server.run());

    let client = Client::connect_tcp("127.0.0.1:8080", EchoService).await?;
    let result = client
        .send_request("echo".to_string(), vec![Value::String("Hello".into())])
        .await?;
    println!("Result: {:?}", result);
    Ok(())
}
```
