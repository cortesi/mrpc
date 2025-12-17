//! Conformance tests for mrpc interoperability with the msgpack-rpc crate.

#![allow(clippy::tests_outside_test_module)]

use std::{error::Error, sync::Arc, time::Duration};

use async_trait::async_trait;
use mrpc::{Connection, Result as MrpcResult, RpcError, RpcSender, Server, Value};
use tempfile::tempdir;
use tokio::{net::UnixStream, sync::Mutex, time::sleep};
use tokio_util::compat::TokioAsyncReadCompatExt;

const PINGS: u32 = 3;

#[derive(Clone, Default)]
struct PingPongService {
    pong_counter: Arc<Mutex<u32>>,
}

#[async_trait]
impl Connection for PingPongService {
    async fn handle_request(
        &self,
        _client: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> MrpcResult<Value> {
        match method {
            "ping" => {
                let _ = params.first().and_then(|v| v.as_i64()).ok_or_else(|| {
                    RpcError::Protocol("Expected integer parameter for ping".into())
                })?;
                let mut count = self.pong_counter.lock().await;
                *count += 1;
                Ok(Value::Boolean(true))
            }
            _ => Err(RpcError::Protocol(format!(
                "PingPongService: Unknown method: {}",
                method
            ))),
        }
    }
}

#[tokio::test]
async fn test_mrpc_compatibility_with_msgpack_rpc() -> Result<(), Box<dyn Error>> {
    let temp_dir = tempdir()?;
    let socket_path = temp_dir.path().join("pingpong.sock");

    // Set up the mrpc server
    let pong_counter = Arc::new(Mutex::new(0_u32));
    let pong_counter_clone = pong_counter.clone();

    let server: Server<PingPongService> = Server::from_fn(move || PingPongService {
        pong_counter: pong_counter_clone.clone(),
    })
    .unix(&socket_path)
    .await?;

    let server_task = tokio::spawn(async move {
        if let Err(e) = server.run().await {
            panic!("Server error: {}", e);
        }
    });

    // Give the server a moment to start up
    sleep(Duration::from_millis(100)).await;

    // Create a msgpack-rpc client
    let socket = UnixStream::connect(&socket_path).await?;
    let client = msgpack_rpc::Client::new(socket.compat());

    for i in 0..PINGS {
        client
            .request("ping", &[msgpack_rpc::Value::Integer(i.into())])
            .await
            .unwrap();
        // Add a small delay between requests
        sleep(Duration::from_millis(10)).await;
    }

    // Give more time for all pings to be processed
    sleep(Duration::from_millis(500)).await;

    // Check if our server received 10 pings
    let final_count = *pong_counter.lock().await;
    assert_eq!(
        final_count, PINGS,
        "Expected {} pongs, but got {}",
        PINGS, final_count
    );

    server_task.abort();
    Ok(())
}
