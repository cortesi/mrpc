//! Ping-pong integration tests for mrpc bidirectional communication.

#![allow(clippy::tests_outside_test_module)]

use std::{error::Error, sync::Arc, time::Duration};

use async_trait::async_trait;
use mrpc::{self, Client, Connection, RpcError, RpcSender, Server};
use rmpv::Value;
use tempfile::tempdir;
use tokio::{
    sync::{Mutex, oneshot},
    time::timeout,
};
use tracing_test::traced_test;

#[derive(Clone)]
struct PingService {
    pong_count: Arc<Mutex<i32>>,
    connected_count: Arc<Mutex<i32>>,
    done_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    target_pongs: i32,
}

#[async_trait]
impl Connection for PingService {
    async fn connected(&self, _client: RpcSender) -> mrpc::Result<()> {
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
        Err(RpcError::Protocol(
            format!("PingService: Unknown method: {}", method).into(),
        ))
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
            if *count == self.target_pongs {
                let done_tx = self.done_tx.lock().await.take();
                if let Some(done_tx) = done_tx {
                    let _send_result = done_tx.send(());
                }
            }
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
    async fn connected(&self, _client: RpcSender) -> mrpc::Result<()> {
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
            _ => Err(RpcError::Protocol(
                format!("PongService: Unknown method: {}", method).into(),
            )),
        }
    }
}

#[traced_test]
#[tokio::test]
async fn test_pingpong() -> Result<(), Box<dyn Error>> {
    let temp_dir = tempdir()?;
    let socket_path = temp_dir.path().join("pong.sock");

    // Set up the Pong server
    let server: Server<PongService> = Server::from_fn(PongService::default)
        .unix(&socket_path)
        .await?;
    let server_handle = server.spawn().await?;

    // Set up the Ping client
    let pong_count = Arc::new(Mutex::new(0));
    let num_pings = 5;
    let (done_tx, done_rx) = oneshot::channel();
    let ping_service = PingService {
        pong_count: pong_count.clone(),
        connected_count: Arc::new(Mutex::new(0)),
        done_tx: Arc::new(Mutex::new(Some(done_tx))),
        target_pongs: num_pings,
    };
    let client = Client::connect_unix(&socket_path, ping_service.clone()).await?;

    // Start the ping-pong process
    for _ in 0..num_pings {
        client.send_request("ping", &[]).await?;
    }

    timeout(Duration::from_secs(5), done_rx)
        .await
        .map_err(|_| RpcError::Protocol("Timed out waiting for pongs".into()))?
        .map_err(|_| RpcError::Protocol("Pong signal dropped unexpectedly".into()))?;

    let final_pong_count = *pong_count.lock().await;
    let final_connected_count = *ping_service.connected_count.lock().await;

    assert_eq!(final_pong_count, num_pings, "Pong count mismatch");
    assert_eq!(final_connected_count, 1, "Connected count mismatch");

    drop(client);
    server_handle.shutdown();
    server_handle.join().await?;
    Ok(())
}
