//! Typed service dispatch: one serde-tagged enum carries the method-to-type
//! mapping for both the client and the server.

#![allow(clippy::tests_outside_test_module)]

use async_trait::async_trait;
use mrpc::{
    Client, Connection, Result, RpcError, RpcSender, Server, ServiceCall, Value, decode_request,
    serialize_value,
};
use serde::{Deserialize, Serialize};

/// The service's request table. Variant names are the wire methods.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DemoRequest {
    Ping,
    Greet(GreetRequest),
    Add(i64, i64),
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct GreetRequest {
    name: String,
}

impl ServiceCall for GreetRequest {
    const METHOD: &'static str = "greet";
    type Response = String;
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct PingRequest;

impl ServiceCall for PingRequest {
    const METHOD: &'static str = "ping";
    type Response = String;
}

#[derive(Clone, Default)]
struct DemoService;

#[async_trait]
impl Connection for DemoService {
    async fn handle_request(
        &self,
        _sender: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        let request: DemoRequest = decode_request(method, &params)?;
        match request {
            DemoRequest::Ping => serialize_value(&"pong"),
            DemoRequest::Greet(request) => serialize_value(&format!("hello {}", request.name)),
            DemoRequest::Add(a, b) => serialize_value(&(a + b)),
        }
    }
}

#[test]
fn decode_selects_variants_by_method() {
    let ping: DemoRequest = decode_request("ping", &[]).expect("unit variant decodes");
    assert_eq!(ping, DemoRequest::Ping);

    let params = mrpc::serialize_params(&GreetRequest {
        name: "ada".to_owned(),
    })
    .expect("params serialize");
    let greet: DemoRequest = decode_request("greet", &params).expect("struct variant decodes");
    assert_eq!(
        greet,
        DemoRequest::Greet(GreetRequest {
            name: "ada".to_owned()
        })
    );

    let params = mrpc::serialize_params(&(2_i64, 3_i64)).expect("params serialize");
    let add: DemoRequest = decode_request("add", &params).expect("tuple variant decodes");
    assert_eq!(add, DemoRequest::Add(2, 3));
}

#[test]
fn decode_maps_an_unknown_method_to_method_not_found() {
    let error = decode_request::<DemoRequest>("missing", &[]).expect_err("unknown method fails");

    let RpcError::Service(service) = error else {
        panic!("unknown method must map to a service error: {error}");
    };
    assert_eq!(service.name, "MethodNotFound");
}

#[test]
fn decode_keeps_a_payload_mismatch_as_a_deserialization_error() {
    let params = vec![Value::from(42)];

    let error = decode_request::<DemoRequest>("greet", &params).expect_err("wrong payload fails");

    assert!(
        matches!(error, RpcError::ResponseDeserialization(_)),
        "payload mismatches must not report a missing method: {error}"
    );
}

#[tokio::test]
async fn call_service_round_trips_through_decode_request() {
    let server: Server<DemoService> = Server::from_fn(DemoService::default)
        .tcp("127.0.0.1:0")
        .await
        .expect("server binds");
    let addr = server.local_addr().expect("local addr");
    tokio::spawn(server.run());

    let client = Client::connect_tcp(&addr.to_string(), ())
        .await
        .expect("client connects");

    let pong = client
        .call_service(&PingRequest)
        .await
        .expect("ping succeeds");
    assert_eq!(pong, "pong");

    let greeting = client
        .call_service(&GreetRequest {
            name: "ada".to_owned(),
        })
        .await
        .expect("greet succeeds");
    assert_eq!(greeting, "hello ada");
}
