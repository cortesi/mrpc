//! Simple Unix socket client example.

use std::{error::Error, result};

use mrpc::Client;
use rmpv::Value;

#[tokio::main]
async fn main() -> result::Result<(), Box<dyn Error>> {
    let client = Client::connect_unix("/tmp/example_socket", ()).await?;
    let result = client
        .send_request("echo", &[Value::String("Hello, RPC Server!".into())])
        .await?;
    println!("Received response: {:?}", result);
    Ok(())
}
