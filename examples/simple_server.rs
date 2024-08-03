use mrpc::{Connection, Result, RpcSender, Server};
use rmpv::Value;
use std::error::Error;

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
async fn main() -> std::result::Result<(), Box<dyn Error>> {
    let socket_path = "/tmp/example_socket";
    // Remove the socket file if it already exists
    if std::path::Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)?;
    }
    let server = Server::from_closure(SimpleService::default)
        .unix(socket_path)
        .await?;
    println!("Server listening on {}", socket_path);
    server.run().await?;
    Ok(())
}
