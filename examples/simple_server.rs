use mrpc::{Result, RpcHandle, RpcService, Server};
use rmpv::Value;
use std::error::Error;

#[derive(Clone)]
struct SimpleService;

#[async_trait::async_trait]
impl RpcService for SimpleService {
    async fn handle_request<S>(
        &self,
        _: RpcHandle,
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
    let server = Server::new(SimpleService).unix(socket_path).await?;
    println!("Server listening on {}", socket_path);
    server.run().await?;
    Ok(())
}
