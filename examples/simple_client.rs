use mrpc::{Client, RpcSender, RpcService};
use rmpv::Value;
use std::error::Error;

// We need to define a dummy service even for a client
#[derive(Clone)]
struct DummyClientService;

#[async_trait::async_trait]
impl RpcService for DummyClientService {}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn Error>> {
    // Connect to the server
    let client = Client::connect_unix("/tmp/example_socket", DummyClientService).await?;

    // Send a message
    let result = client
        .send_request(
            "echo".to_string(),
            vec![Value::String("Hello, RPC Server!".into())],
        )
        .await?;

    println!("Received response: {:?}", result);

    Ok(())
}
