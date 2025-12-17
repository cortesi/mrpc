//! Simple echo server and client example.

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
