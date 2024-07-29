use async_trait::async_trait;
use mrpc::{self, Client, Connection, RpcError, RpcSender, Server};
use rmpv::Value;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncRead, AsyncWrite};
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

    async fn handle_request<S>(
        &mut self,
        _sender: RpcSender,
        method: &str,
        _params: Vec<Value>,
    ) -> mrpc::Result<Value>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Err(RpcError::Protocol(format!(
            "PingService: Unknown method: {}",
            method
        )))
    }

    async fn handle_notification<S>(
        &mut self,
        _sender: RpcSender,
        method: &str,
        _params: Vec<Value>,
    ) -> mrpc::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
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

    async fn handle_request<S>(
        &mut self,
        sender: RpcSender,
        method: &str,
        _params: Vec<Value>,
    ) -> mrpc::Result<Value>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
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
            eprintln!("Server error: {}", e);
        }
    });

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

    assert_eq!(*pong_count.lock().await, num_pings);
    // Assert that the connected method was called for both client and server
    assert_eq!(*ping_service.connected_count.lock().await, 1);

    pong_server_task.abort();
    Ok(())
}
