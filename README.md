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
use mrpc::{Client, Connection, Result, RpcSender, Server};
use rmpv::Value;

#[derive(Clone, Default)]
struct EchoService;

#[async_trait::async_trait]
impl Connection for EchoService {
    async fn handle_request(
        &mut self,
        _: RpcSender,
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
    let server = Server::from_closure(EchoService::default)
        .tcp("127.0.0.1:0")
        .await?;
    let addr = server.local_addr().unwrap();
    tokio::spawn(server.run());
    let client = Client::connect_tcp(&addr.to_string(), EchoService).await?;
    let result = client
        .send_request("echo", &[Value::String("Hello".into())])
        .await?;
    println!("Result: {:?}", result);
    Ok(())
}
```
