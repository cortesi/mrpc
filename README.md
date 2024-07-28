[![Crates.io](https://img.shields.io/crates/v/mrpc.svg)](https://crates.io/crates/mrpc)
[![Documentation](https://docs.rs/mrpc/badge.svg)](https://docs.rs/mrpc)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)


# mrpc

A MessagePack-RPC implementation in Rust.

## Features

- Asynchronous RPC servers and clients
- Support for TCP and Unix domain sockets
- Full MessagePack-RPC spec implementation (requests, responses, notifications)
- Bidirectional communication support
- Built on `tokio` for async I/O
- Uses `rmpv` for MessagePack serialization

## Quick Start

Add this to your `Cargo.toml`:

```toml
[dependencies]
rust-msgpack-rpc = "0.1.0"
```

### Implementing a Server

1. Implement the `RpcService` trait:

```rust
use rust_msgpack_rpc::{RpcService, RpcSender, Result};
use rmpv::Value;

struct MyService;

#[async_trait::async_trait]
impl RpcService for MyService {
    async fn handle_request<S>(&self, _client: RpcSender, method: &str, params: Vec<Value>) -> Result<Value> {
        // Implement your RPC methods here
    }
}
```

2. Create and run the server:

```rust
use rust_msgpack_rpc::Server;

#[tokio::main]
async fn main() -> Result<()> {
    let service = MyService;
    let server = Server::new(service).tcp("127.0.0.1:8080").await?;
    server.run().await
}
```

### Implementing a Client

```rust
use rust_msgpack_rpc::Client;

#[tokio::main]
async fn main() -> Result<()> {
    let client = Client::connect_tcp("127.0.0.1:8080", ()).await?;
    let result = client.send_request("method_name".to_string(), vec![]).await?;
    println!("Result: {:?}", result);
}
```

## Bidirectional Communication

Both servers and clients can handle incoming RPC messages. To enable this for a
client, implement `RpcService` for it just like you would for a server.

## License

This project is licensed under [MIT License](LICENSE).

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
