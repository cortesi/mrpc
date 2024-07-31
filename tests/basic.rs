use async_trait::async_trait;
use mrpc::{Client, Connection, Result, RpcError, RpcSender, Server, ServiceError, Value};

struct TestService;

#[async_trait]
#[async_trait]
impl Connection for TestService {
    async fn handle_request(
        &mut self,
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

async fn setup_server_and_client() -> Result<(Client<TestService>, Server<TestService>)> {
    let server = Server::from_closure(|| TestService)
        .tcp("127.0.0.1:0")
        .await?;
    let addr = server.local_addr()?;

    let _server_handle = tokio::spawn(async move {
        server.run().await.unwrap();
    });

    let client = Client::connect_tcp(&addr.to_string(), TestService).await?;

    Ok((client, Server::from_closure(|| TestService)))
}

#[tokio::test]
async fn test_basic_request_response() -> Result<()> {
    let (client, _) = setup_server_and_client().await?;

    let result = client
        .send_request("add", &[Value::from(5), Value::from(3)])
        .await?;
    assert_eq!(result, Value::from(8));

    Ok(())
}

#[tokio::test]
async fn test_method_not_found() -> Result<()> {
    let (client, _) = setup_server_and_client().await?;

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
