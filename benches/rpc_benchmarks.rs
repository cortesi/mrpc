use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mrpc::{Client, Result, RpcError, RpcHandle, RpcSender, RpcService, Server};
use rmpv::Value;
use std::path::PathBuf;
use tempfile::tempdir;
use tokio::runtime::Runtime;

#[derive(Clone)]
struct BenchService;

#[async_trait::async_trait]
impl RpcService for BenchService {
    async fn handle_request<S>(
        &self,
        _: RpcHandle,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        match method {
            "echo" => Ok(params[0].clone()),
            "add" => {
                let a = params[0].as_i64().unwrap();
                let b = params[1].as_i64().unwrap();
                Ok(Value::Integer((a + b).into()))
            }
            _ => Err(RpcError::Protocol(format!("Unknown method: {}", method))),
        }
    }
}

fn create_unix_socket_path() -> (PathBuf, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("benchmark.sock");
    (path, dir)
}

fn run_in_tokio<F: std::future::Future>(f: F) -> F::Output {
    let rt = Runtime::new().unwrap();
    rt.block_on(f)
}

fn bench_echo(c: &mut Criterion) {
    c.bench_function("echo", |b| {
        b.iter(|| {
            run_in_tokio(async {
                let (socket_path, _temp_dir) = create_unix_socket_path();

                let socket_path_clone = socket_path.clone();
                let server = Server::new(BenchService)
                    .unix(socket_path_clone)
                    .await
                    .unwrap();

                let server_handle = tokio::spawn(async move {
                    server.run().await.unwrap();
                });

                let client = Client::connect_unix(&socket_path, BenchService)
                    .await
                    .unwrap();

                let result = client
                    .send_request("echo".to_string(), vec![Value::String("hello".into())])
                    .await
                    .unwrap();

                server_handle.abort();

                black_box(result)
            })
        })
    });
}

fn bench_add(c: &mut Criterion) {
    c.bench_function("add", |b| {
        b.iter(|| {
            run_in_tokio(async {
                let (socket_path, _temp_dir) = create_unix_socket_path();

                let socket_path_clone = socket_path.clone();
                let server = Server::new(BenchService)
                    .unix(socket_path_clone)
                    .await
                    .unwrap();

                let server_handle = tokio::spawn(async move {
                    server.run().await.unwrap();
                });

                let client = Client::connect_unix(&socket_path, BenchService)
                    .await
                    .unwrap();

                let result = client
                    .send_request(
                        "add".to_string(),
                        vec![Value::Integer(40.into()), Value::Integer(2.into())],
                    )
                    .await
                    .unwrap();

                server_handle.abort();

                black_box(result)
            })
        })
    });
}

criterion_group!(benches, bench_echo, bench_add);
criterion_main!(benches);
