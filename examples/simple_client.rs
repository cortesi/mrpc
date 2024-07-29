use mrpc::{Client, Connection};
use rmpv::Value;
use std::error::Error;

// We need to define a dummy service even for a client
#[derive(Clone, Default)]
struct DummyClientService;

#[async_trait::async_trait]
impl Connection for DummyClientService {}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn Error>> {
    let client = Client::connect_unix("/tmp/example_socket", DummyClientService).await?;
    let result = client
        .send_request("echo", &[Value::String("Hello, RPC Server!".into())])
        .await?;
    println!("Received response: {:?}", result);
    Ok(())
}
