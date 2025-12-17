//! Basic integration tests for mrpc.

#![allow(clippy::tests_outside_test_module)]

use std::{future::pending, sync::Arc, time::Duration};

use async_trait::async_trait;
use mrpc::{
    Client, Connection, Listener, Result, RpcError, RpcSender, Server, ServerHandle, ServiceError,
    Value, duplex,
};
use tokio::{
    io::DuplexStream,
    sync::{Mutex, oneshot},
    task,
    time::{sleep, timeout},
};

struct OnceListener {
    stream: Mutex<Option<DuplexStream>>,
}

#[async_trait]
impl Listener for OnceListener {
    type Stream = DuplexStream;

    async fn accept(&self) -> Result<Self::Stream> {
        let stream = { self.stream.lock().await.take() };
        match stream {
            Some(stream) => Ok(stream),
            None => pending::<Result<Self::Stream>>().await,
        }
    }
}

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
    connected_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl TestClientConnect {
    fn new() -> (Self, oneshot::Receiver<()>) {
        let (connected_tx, connected_rx) = oneshot::channel();
        (
            Self {
                connected_tx: Arc::new(Mutex::new(Some(connected_tx))),
            },
            connected_rx,
        )
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

        let connected_tx = self.connected_tx.lock().await.take();
        if let Some(connected_tx) = connected_tx {
            let _send_result = connected_tx.send(());
        }

        Ok(())
    }
}

async fn setup_server_and_client<T: Connection + Default>() -> Result<(Client<T>, ServerHandle)> {
    let server = Server::from_fn(|| TestServer).tcp("127.0.0.1:0").await?;
    let server_handle = server.spawn().await?;
    let addr = server_handle.local_addr()?;

    let client = Client::connect_tcp(&addr.to_string(), T::default()).await?;

    Ok((client, server_handle))
}

async fn setup_server_and_client_with_connect() -> Result<(
    Client<TestClientConnect>,
    ServerHandle,
    oneshot::Receiver<()>,
)> {
    let (test_client, connected_rx) = TestClientConnect::new();
    let server = Server::from_fn(|| TestServer).tcp("127.0.0.1:0").await?;
    let server_handle = server.spawn().await?;
    let addr = server_handle.local_addr()?;

    let client = Client::connect_tcp(&addr.to_string(), test_client).await?;

    Ok((client, server_handle, connected_rx))
}

#[tokio::test]
async fn test_basic_request_response() -> Result<()> {
    let (client, server_handle) = setup_server_and_client::<TestClient>().await?;

    let result = client
        .send_request("add", &[Value::from(5), Value::from(3)])
        .await?;
    assert_eq!(result, Value::from(8));

    server_handle.shutdown();
    server_handle.join().await?;

    Ok(())
}

#[cfg(feature = "serde")]
#[tokio::test]
async fn test_typed_call() -> Result<()> {
    let (client, server_handle) = setup_server_and_client::<TestClient>().await?;

    let result: i64 = client.call("add", &(5_i64, 3_i64)).await?;
    assert_eq!(result, 8);

    server_handle.shutdown();
    server_handle.join().await?;

    Ok(())
}

#[tokio::test]
async fn test_method_not_found() -> Result<()> {
    let (client, server_handle) = setup_server_and_client::<TestClient>().await?;

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

    server_handle.shutdown();
    server_handle.join().await?;

    Ok(())
}

#[tokio::test]
async fn test_concurrent_requests() -> Result<()> {
    let (client, server_handle) = setup_server_and_client::<TestClient>().await?;
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

    server_handle.shutdown();
    server_handle.join().await?;

    Ok(())
}

#[tokio::test]
async fn test_duplex_transport() -> Result<()> {
    let (client_stream, server_stream) = duplex(1024);
    let server = Server::from_fn(|| TestServer).with_listener(OnceListener {
        stream: Mutex::new(Some(server_stream)),
    })?;
    let server_handle = server.spawn().await?;

    let client = Client::from_stream(client_stream, ()).await?;
    let result = client
        .send_request("add", &[Value::from(2), Value::from(4)])
        .await?;
    assert_eq!(result, Value::from(6));

    drop(client);
    server_handle.shutdown();
    server_handle.join().await?;
    Ok(())
}

#[tokio::test]
async fn test_client_request_from_connected() -> Result<()> {
    let timeout_duration = Duration::from_secs(5); // 5 second timeout

    let result = timeout(timeout_duration, async {
        let (_client, server_handle, connected_rx) = setup_server_and_client_with_connect().await?;

        let connected = connected_rx
            .await
            .map_err(|_| RpcError::Protocol("Connected method dropped unexpectedly".into()));

        server_handle.shutdown();
        server_handle.join().await?;

        connected
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(RpcError::Protocol("Test timed out".into())),
    }
}
