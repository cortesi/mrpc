//! Simple Unix socket server example.

use std::{error::Error, fs, path::Path, result};

use mrpc::{Connection, Result, RpcSender, Server};
use rmpv::Value;

/// Simple echo service.
#[derive(Clone, Default)]
struct SimpleService;

#[async_trait::async_trait]
impl Connection for SimpleService {
    async fn handle_request(
        &self,
        _: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        match method {
            "echo" => {
                println!("Received echo request with params: {:?}", params);
                Ok(params[0].clone())
            }
            _ => Err(mrpc::RpcError::Protocol(format!(
                "Unknown method: {}",
                method
            ))),
        }
    }
}

#[tokio::main]
async fn main() -> result::Result<(), Box<dyn Error>> {
    let socket_path = "/tmp/example_socket";
    // Remove the socket file if it already exists
    if Path::new(socket_path).exists() {
        fs::remove_file(socket_path)?;
    }
    let server = Server::from_fn(SimpleService::default)
        .unix(socket_path)
        .await?;
    println!("Server listening on {}", socket_path);
    server.run().await?;
    Ok(())
}
