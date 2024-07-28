use async_trait::async_trait;
use mrpc::{self, Client, RpcError, RpcHandle, RpcSender, RpcService, Server};
use rmpv::Value;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tracing_test::traced_test;

#[derive(Clone)]
struct TestService {
    notification_count: Arc<Mutex<i32>>,
}

#[async_trait]
impl RpcService for TestService {
    async fn handle_request<S>(
        &self,
        _sender: RpcHandle,
        method: &str,
        params: Vec<Value>,
    ) -> mrpc::Result<Value>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        match method {
            "echo" => Ok(params.into_iter().next().unwrap_or(Value::Nil)),
            "add" => {
                let sum: i64 = params.into_iter().filter_map(|v| v.as_i64()).sum();
                Ok(Value::Integer(sum.into()))
            }
            "get_notification_count" => {
                let count = *self.notification_count.lock().await;
                Ok(Value::Integer(count.into()))
            }
            _ => Err(RpcError::Protocol(format!("Unknown method: {}", method))),
        }
    }

    async fn handle_notification<S>(
        &self,
        _sender: RpcHandle,
        method: &str,
        params: Vec<Value>,
    ) -> mrpc::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        println!(
            "Received notification: {} with params: {:?}",
            method, params
        );
        if method == "test_notification" {
            let mut count = self.notification_count.lock().await;
            *count += 1;
        }
        Ok(())
    }
}

#[traced_test]
#[tokio::test]
async fn test_rpc_service() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempdir()?;
    let socket_path = temp_dir.path().join("test.sock");

    // Set up the server
    let service = TestService {
        notification_count: Arc::new(Mutex::new(0)),
    };
    let server = Server::new(service.clone()).unix(&socket_path).await?;
    let server_task = tokio::spawn(async move {
        let e = server.run().await;
        if let Err(e) = e {
            eprintln!("Server error: {}", e);
        }
    });

    let client = Client::connect_unix(&socket_path, service.clone()).await?;

    // Test echo request
    let echo_result = client
        .send_request("echo", &[Value::String("Hello, RPC!".into())])
        .await?;
    assert_eq!(echo_result, Value::String("Hello, RPC!".into()));

    // Test add request
    let add_result = client
        .send_request("add", &[Value::Integer(5.into()), Value::Integer(7.into())])
        .await?;
    assert_eq!(add_result, Value::Integer(12.into()));

    // Test notification
    client
        .send_notification(
            "test_notification",
            &[Value::String("Test notification".into())],
        )
        .await?;

    // Wait a bit for the notification to be processed
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Check notification count
    let count_result = client.send_request("get_notification_count", &[]).await?;
    assert_eq!(count_result, Value::Integer(1.into()));

    // Test unknown method
    let unknown_result = client.send_request("unknown", &[]).await;
    assert!(unknown_result.is_err());

    // Clean up
    server_task.abort();

    Ok(())
}
