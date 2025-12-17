//! Basic integration tests for mrpc.

#![allow(clippy::tests_outside_test_module)]

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use mrpc::{Client, Connection, Result, RpcError, RpcSender, Server, ServiceError, Value};
use tokio::{
    sync::Mutex,
    task,
    time::{sleep, timeout},
};

#[derive(Clone)]
struct TestServer;

#[async_trait]
impl Connection for TestServer {
    async fn handle_request(
        &self,
        _: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        match method {
            "add" => {
                if let [a, b] = params.as_slice() {
                    let a = a.as_i64().ok_or_else(|| {
                        RpcError::Protocol("First parameter must be an integer".into())
                    })?;
                    let b = b.as_i64().ok_or_else(|| {
                        RpcError::Protocol("Second parameter must be an integer".into())
                    })?;
                    Ok(Value::from(a + b))
                } else {
                    Err(RpcError::Protocol("Expected two parameters".into()))
                }
            }
            _ => Err(RpcError::Service(ServiceError {
                name: "MethodNotFound".into(),
                value: Value::String(format!("Method '{}' not found", method).into()),
            })),
        }
    }
}

#[derive(Clone)]
struct TestClient;

impl Default for TestClient {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Connection for TestClient {}

#[derive(Clone)]
struct TestClientConnect {
    connected_success: Arc<Mutex<bool>>,
}

impl TestClientConnect {
    fn new() -> Self {
        Self {
            connected_success: Arc::new(Mutex::new(false)),
        }
    }
}

impl Default for TestClientConnect {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connection for TestClientConnect {
    async fn connected(&self, client: RpcSender) -> Result<()> {
        // Send a message during connection
        let result = client
            .send_request("add", &[Value::from(10), Value::from(20)])
            .await?;
        assert_eq!(result, Value::from(30), "Connected method request failed");

        // Set the flag to indicate successful completion
        let mut success = self.connected_success.lock().await;
        *success = true;

        Ok(())
    }
}

async fn setup_server_and_client<T: Connection + Default>()
-> Result<(Client<T>, Server<TestServer>)> {
    let server = Server::from_fn(|| TestServer).tcp("127.0.0.1:0").await?;
    let addr = server.local_addr()?;

    let _server_handle = tokio::spawn(async move {
        server.run().await.unwrap();
    });

    let client = Client::connect_tcp(&addr.to_string(), T::default()).await?;

    Ok((client, Server::from_fn(|| TestServer)))
}

async fn setup_server_and_client_with_connect() -> Result<(
    Client<TestClientConnect>,
    Server<TestServer>,
    Arc<Mutex<bool>>,
)> {
    let test_client = TestClientConnect::new();
    let connected_success = test_client.connected_success.clone();

    let server = Server::from_fn(|| TestServer).tcp("127.0.0.1:0").await?;
    let addr = server.local_addr()?;

    let _server_handle = tokio::spawn(async move {
        server.run().await.unwrap();
    });

    let client = Client::connect_tcp(&addr.to_string(), test_client).await?;

    Ok((client, Server::from_fn(|| TestServer), connected_success))
}

#[tokio::test]
async fn test_basic_request_response() -> Result<()> {
    let (client, _) = setup_server_and_client::<TestClient>().await?;

    let result = client
        .send_request("add", &[Value::from(5), Value::from(3)])
        .await?;
    assert_eq!(result, Value::from(8));

    Ok(())
}

#[cfg(feature = "serde")]
#[tokio::test]
async fn test_typed_call() -> Result<()> {
    let (client, _) = setup_server_and_client::<TestClient>().await?;

    let result: i64 = client.call("add", &(5_i64, 3_i64)).await?;
    assert_eq!(result, 8);

    Ok(())
}

#[tokio::test]
async fn test_method_not_found() -> Result<()> {
    let (client, _) = setup_server_and_client::<TestClient>().await?;

    let result = client
        .send_request("non_existent_method", &[Value::from(1)])
        .await;

    match result {
        Err(RpcError::Service(ServiceError { name, value })) => {
            assert_eq!(name, "MethodNotFound");
            assert_eq!(
                value,
                Value::String("Method 'non_existent_method' not found".into())
            );
        }
        _ => panic!("Expected Service error, got {:?}", result),
    }

    Ok(())
}

#[tokio::test]
async fn test_concurrent_requests() -> Result<()> {
    let (client, _) = setup_server_and_client::<TestClient>().await?;
    let client = Arc::new(client);

    let num_requests = 100;
    let mut handles = vec![];

    for i in 0..num_requests {
        let client_clone = client.clone();
        let handle = task::spawn(async move {
            sleep(Duration::from_millis(i % 10)).await;
            let result = client_clone
                .send_request("add", &[Value::from(i), Value::from(i)])
                .await?;
            assert_eq!(result, Value::from(i * 2));
            Ok::<_, RpcError>(())
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap()?;
    }

    Ok(())
}

#[tokio::test]
async fn test_client_request_from_connected() -> Result<()> {
    let timeout_duration = Duration::from_secs(5); // 5 second timeout

    let result = timeout(timeout_duration, async {
        let (_client, _, connected_success) = setup_server_and_client_with_connect().await?;

        // Wait for the connected method to complete or timeout
        for _ in 0..50 {
            // Check every 100ms for 5 seconds
            if *connected_success.lock().await {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }

        Err(RpcError::Protocol(
            "Connected method did not complete in time".into(),
        ))
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(RpcError::Protocol("Test timed out".into())),
    }
}
