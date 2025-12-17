//! Typed calls using `mrpc`'s `serde` feature.

use mrpc::{
    Client, Connection, Result, RpcSender, Server, Value, deserialize_param, deserialize_params,
    serialize_value,
};

// snips-start: server
/// Typed handler for a positional-params method.
fn add(params: Vec<Value>) -> Result<Value> {
    let (a, b): (i64, i64) = deserialize_params(params)?;
    serialize_value(&(a + b))
}

/// Typed handler for a single-param notification.
fn log(params: Vec<Value>) -> Result<()> {
    let message: String = deserialize_param(params)?;
    println!("{message}");
    Ok(())
}
// snips-end: server

/// Example service with typed request decoding.
#[derive(Clone, Default)]
struct SerdeService;

#[async_trait::async_trait]
impl Connection for SerdeService {
    async fn handle_request(
        &self,
        _: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        match method {
            "add" => add(params),
            _ => unreachable!(),
        }
    }

    async fn handle_notification(
        &self,
        _: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<()> {
        match method {
            "log" => log(params),
            _ => Ok(()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let server = Server::from_fn(SerdeService::default)
        .tcp("127.0.0.1:0")
        .await?;
    let addr = server.local_addr()?;
    tokio::spawn(server.run());

    let client = Client::connect_tcp(&addr.to_string(), ()).await?;

    // snips-start: client
    client.notify("log", &"hello from serde").await?;
    let sum: i64 = client.call("add", &(5_i64, 3_i64)).await?;
    println!("{sum}");
    // snips-end: client
    Ok(())
}
