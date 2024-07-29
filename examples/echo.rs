use mrpc::{Client, Connection, Result, RpcSender, Server};
use rmpv::Value;

#[derive(Clone, Default)]
struct EchoService;

#[async_trait::async_trait]
impl Connection for EchoService {
    async fn handle_request<S>(
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
        .tcp("127.0.0.1:8080")
        .await?;
    tokio::spawn(server.run());

    let client = Client::connect_tcp("127.0.0.1:8080", EchoService).await?;
    let result = client
        .send_request("echo", &[Value::String("Hello".into())])
        .await?;
    println!("Result: {:?}", result);
    Ok(())
}
