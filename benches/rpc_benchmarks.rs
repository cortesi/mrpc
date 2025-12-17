//! Benchmarks for RPC operations.

use std::{future::Future, hint::black_box, path::PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use mrpc::{Client, Connection, Result, RpcError, RpcSender, Server};
use rmpv::Value;
use tempfile::tempdir;
use tokio::runtime::Runtime;

/// A simple service for benchmarking RPC operations.
#[derive(Clone, Default)]
struct BenchService;

#[async_trait::async_trait]
impl Connection for BenchService {
    async fn handle_request(
        &self,
        _: RpcSender,
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
            _ => Err(RpcError::Protocol(
                format!("Unknown method: {}", method).into(),
            )),
        }
    }
}

/// Creates a temporary Unix socket path for benchmarking.
fn create_unix_socket_path() -> (PathBuf, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("benchmark.sock");
    (path, dir)
}

/// Runs an async future in a new tokio runtime.
fn run_in_tokio<F: Future>(f: F) -> F::Output {
    let rt = Runtime::new().unwrap();
    rt.block_on(f)
}

/// Benchmarks the echo RPC method.
fn bench_echo(c: &mut Criterion) {
    c.bench_function("echo", |b| {
        b.iter(|| {
            run_in_tokio(async {
                let (socket_path, _temp_dir) = create_unix_socket_path();

                let socket_path_clone = socket_path.clone();
                let server = Server::from_fn(BenchService::default)
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
                    .send_request("echo", &[Value::String("hello".into())])
                    .await
                    .unwrap();

                server_handle.abort();

                black_box(result)
            })
        })
    });
}

/// Benchmarks the add RPC method.
fn bench_add(c: &mut Criterion) {
    c.bench_function("add", |b| {
        b.iter(|| {
            run_in_tokio(async {
                let (socket_path, _temp_dir) = create_unix_socket_path();

                let socket_path_clone = socket_path.clone();
                let server = Server::from_fn(BenchService::default)
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
                        "add",
                        &[Value::Integer(40.into()), Value::Integer(2.into())],
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
