use async_trait::async_trait;
use mrpc::{self, Client, Connection, RpcError, RpcSender, Server};
use rmpv::Value;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Mutex;
use tracing_test::traced_test;

#[derive(Clone, Default)]
struct PingService {
    pong_count: Arc<Mutex<i32>>,
    connected_count: Arc<Mutex<i32>>,
}

#[async_trait]
impl Connection for PingService {
    async fn connected(&mut self, _client: RpcSender) -> mrpc::Result<()> {
        let mut count = self.connected_count.lock().await;
        *count += 1;
        Ok(())
    }

    async fn handle_request(
        &self,
        _sender: RpcSender,
        method: &str,
        _params: Vec<Value>,
    ) -> mrpc::Result<Value> {
        Err(RpcError::Protocol(format!(
            "PingService: Unknown method: {}",
            method
        )))
    }

    async fn handle_notification(
        &self,
        _sender: RpcSender,
        method: &str,
        _params: Vec<Value>,
    ) -> mrpc::Result<()> {
        if method == "pong" {
            let mut count = self.pong_count.lock().await;
            *count += 1;
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
struct PongService {
    connected_count: Arc<Mutex<i32>>,
}

#[async_trait]
impl Connection for PongService {
    async fn connected(&mut self, _client: RpcSender) -> mrpc::Result<()> {
        let mut count = self.connected_count.lock().await;
        *count += 1;
        Ok(())
    }

    async fn handle_request(
        &self,
        sender: RpcSender,
        method: &str,
        _params: Vec<Value>,
    ) -> mrpc::Result<Value> {
        match method {
            "ping" => {
                sender
                    .send_notification("pong", &[Value::String("pong".into())])
                    .await?;
                Ok(Value::Boolean(true))
            }
            _ => Err(RpcError::Protocol(format!(
                "PongService: Unknown method: {}",
                method
            ))),
        }
    }
}

#[traced_test]
#[tokio::test]
async fn test_pingpong() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempdir()?;
    let socket_path = temp_dir.path().join("pong.sock");

    // Set up the Pong server
    let server: Server<PongService> = Server::from_closure(PongService::default)
        .unix(&socket_path)
        .await?;
    let pong_server_task = tokio::spawn(async move {
        let e = server.run().await;
        if let Err(e) = e {
            panic!("Server error: {}", e);
        }
    });

    // Give the server a moment to start up
    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

    // Set up the Ping client
    let pong_count = Arc::new(Mutex::new(0));
    let ping_service = PingService {
        pong_count: pong_count.clone(),
        connected_count: Arc::new(Mutex::new(0)),
    };
    let client = Client::connect_unix(&socket_path, ping_service.clone()).await?;

    // Start the ping-pong process
    let num_pings = 5;
    for _ in 0..num_pings {
        client.send_request("ping", &[]).await?;
    }

    // Give some time for all pongs to be processed
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let final_pong_count = *pong_count.lock().await;
    let final_connected_count = *ping_service.connected_count.lock().await;

    assert_eq!(final_pong_count, num_pings, "Pong count mismatch");
    assert_eq!(final_connected_count, 1, "Connected count mismatch");

    pong_server_task.abort();
    Ok(())
}
