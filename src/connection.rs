//! Core RPC connection handling and message processing.
//!
//! Defines structures and traits for managing RPC connections,
//! handling incoming and outgoing messages, and implementing
//! RPC services.
#[cfg(feature = "serde")]
use std::io::Cursor;
use std::{collections::HashMap, marker::PhantomData, sync::Arc};

use async_trait::async_trait;
use rmpv::Value;
#[cfg(feature = "serde")]
use rmpv::{decode::read_value, encode::write_value};
#[cfg(feature = "serde")]
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, WriteHalf, split},
    runtime::Handle,
    sync::{Mutex, mpsc, oneshot},
};
use tokio_util::io::SyncIoBridge;
use tracing::{error, trace, warn};

use crate::{
    error::{ProtocolError, Result, RpcError, ServiceError},
    message::*,
};

/// Internal message type for communication between the client API and the connection handler.
#[derive(Debug)]
pub enum ClientMessage {
    /// An RPC request with a response channel.
    Request {
        /// Method name.
        method: String,
        /// Method parameters.
        params: Vec<Value>,
        /// Channel for sending the response back.
        response_sender: oneshot::Sender<Result<Value>>,
    },
    /// An RPC notification (no response expected).
    Notification {
        /// Method name.
        method: String,
        /// Method parameters.
        params: Vec<Value>,
    },
}

/// The interface for sending RPC requests and notifications.
#[derive(Debug, Clone)]
pub struct RpcSender {
    /// Channel sender for client messages.
    pub(crate) sender: mpsc::Sender<ClientMessage>,
}

impl RpcSender {
    /// Sends an RPC request and waits for the response.
    pub async fn send_request(&self, method: &str, params: &[Value]) -> Result<Value> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender
            .send(ClientMessage::Request {
                method: method.to_string(),
                params: params.to_vec(),
                response_sender,
            })
            .await
            .map_err(|_| RpcError::Disconnect { source: None })?;
        response_receiver
            .await
            .map_err(|_| RpcError::Disconnect { source: None })?
    }

    /// Sends an RPC notification without waiting for a response.
    pub async fn send_notification(&self, method: &str, params: &[Value]) -> Result<()> {
        self.sender
            .send(ClientMessage::Notification {
                method: method.to_string(),
                params: params.to_vec(),
            })
            .await
            .map_err(|_| RpcError::Disconnect { source: None })
    }

    /// Sends a typed request and deserializes the response.
    #[cfg(feature = "serde")]
    pub async fn call<Req, Resp>(&self, method: &str, req: &Req) -> Result<Resp>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let params = serialize_params(req)?;
        let value = self.send_request(method, &params).await?;
        deserialize_response(&value)
    }

    /// Sends a typed notification.
    #[cfg(feature = "serde")]
    pub async fn notify<Req>(&self, method: &str, req: &Req) -> Result<()>
    where
        Req: Serialize,
    {
        let params = serialize_params(req)?;
        self.send_notification(method, &params).await
    }
}

#[cfg(feature = "serde")]
/// Serializes a typed request into a MessagePack-RPC params array.
///
/// If the encoded value is an array, its elements become the params array. Otherwise, the encoded
/// value is sent as a single parameter.
fn serialize_params<Req>(req: &Req) -> Result<Vec<Value>>
where
    Req: Serialize,
{
    let buf = rmp_serde::to_vec(req)?;
    let value = read_value(&mut Cursor::new(buf))?;
    match value {
        Value::Array(values) => Ok(values),
        value => Ok(vec![value]),
    }
}

#[cfg(feature = "serde")]
/// Deserializes a typed response from a MessagePack value.
fn deserialize_response<Resp>(value: &Value) -> Result<Resp>
where
    Resp: DeserializeOwned,
{
    let mut buf = Vec::new();
    write_value(&mut buf, value)?;
    Ok(rmp_serde::from_slice(&buf)?)
}

/// Handles an RPC connection, processing incoming and outgoing messages.
pub struct ConnectionHandler<S, T: Connection>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// The underlying RPC connection.
    connection: Arc<Mutex<RpcConnection<S>>>,
    /// The service implementation.
    service: Arc<T>,
    /// Sender for outgoing RPC messages.
    rpc_sender: RpcSender,
}

impl<S, T: Connection> ConnectionHandler<S, T>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Creates a new connection handler.
    pub fn new(
        connection: RpcConnection<S>,
        service: T,
        client_sender: mpsc::Sender<ClientMessage>,
    ) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
            service: Arc::new(service),
            rpc_sender: RpcSender {
                sender: client_sender,
            },
        }
    }

    /// Runs the connection handler, processing messages until the connection closes.
    pub async fn run(&mut self, client_receiver: mpsc::Receiver<ClientMessage>) -> Result<()> {
        let (connected_tx, mut connected_rx) = oneshot::channel();
        let rpc_sender_clone = self.rpc_sender.clone();

        // Spawn the connected method in a separate task
        let service = Arc::clone(&self.service);
        tokio::spawn(async move {
            let result = service.connected(rpc_sender_clone).await;
            // Receiver may be dropped if connection handler exits early; ignore send errors.
            drop(connected_tx.send(result));
        });

        let mut connected_done = false;
        let mut receiver = {
            let mut conn = self.connection.lock().await;
            conn.receiver()
        };

        // Clone Arc<Mutex<RpcConnection>> for the client message handling task
        let connection_clone = self.connection.clone();

        // Spawn a task to handle client messages
        let client_handler = tokio::spawn(async move {
            handle_client_messages(connection_clone, client_receiver).await;
        });

        let mut incoming_handlers = Vec::new();

        loop {
            tokio::select! {
                Some(message_result) = receiver.recv() => {
                    match message_result {
                        Ok(message) => {
                            let connection = self.connection.clone();
                            let service = Arc::clone(&self.service);
                            let rpc_sender = self.rpc_sender.clone();
                            let handler = tokio::spawn(async move {
                                if let Err(e) = handle_incoming_message(connection, service, rpc_sender, message).await {
                                    error!("Error handling incoming message: {}", e);
                                    if matches!(e, RpcError::Disconnect { .. }) {
                                        return Err(e);
                                    }
                                }
                                Ok(())
                            });
                            incoming_handlers.push(handler);
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
                connected_result = &mut connected_rx, if !connected_done => {
                    connected_done = true;
                    match connected_result {
                        Ok(Ok(())) => {
                            // Connected method succeeded, continue with the loop
                        }
                        Ok(Err(e)) => {
                            // Connected method returned an error
                            return Err(e);
                        }
                        Err(_) => {
                            // Connected task was cancelled or panicked
                            return Err(RpcError::Protocol("Connected task failed".into()));
                        }
                    }
                }
                else => {
                    break;
                }
            }
        }

        // Cancel the client handler task
        client_handler.abort();

        // Wait for all incoming message handlers to complete
        for handler in incoming_handlers {
            if let Err(e) = handler.await {
                error!("Error joining incoming message handler: {}", e);
            }
        }

        Ok(())
    }
}

/// Handles a single incoming message (request, response, or notification).
async fn handle_incoming_message<S, T>(
    connection: Arc<Mutex<RpcConnection<S>>>,
    service: Arc<T>,
    rpc_sender: RpcSender,
    message: Message,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    T: Connection,
{
    let service = service.as_ref();
    match message {
        Message::Request(request) => {
            let result = service
                .handle_request(rpc_sender.clone(), &request.method, request.params)
                .await;
            let response = match result {
                Ok(value) => Response {
                    id: request.id,
                    result: Ok(value),
                },
                Err(RpcError::Service(service_error)) => {
                    warn!("Service error: {}", service_error);
                    Response {
                        id: request.id,
                        result: Err(service_error.into()),
                    }
                }
                Err(e) => {
                    warn!("RPC error: {}", e);
                    Response {
                        id: request.id,
                        result: Err(Value::String(format!("Internal error: {}", e).into())),
                    }
                }
            };
            let mut conn = connection.lock().await;
            conn.write_message(&Message::Response(response)).await?;
        }
        Message::Notification(notification) => {
            service
                .handle_notification(
                    rpc_sender.clone(),
                    &notification.method,
                    notification.params,
                )
                .await?;
        }
        Message::Response(response) => {
            let mut conn = connection.lock().await;
            if let Err(e) = conn.handle_response(response) {
                warn!("error handling response: {}", e);
            }
        }
    }
    Ok(())
}

/// Processes outgoing client messages from the channel.
async fn handle_client_messages<S>(
    connection: Arc<Mutex<RpcConnection<S>>>,
    mut client_receiver: mpsc::Receiver<ClientMessage>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    while let Some(message) = client_receiver.recv().await {
        let mut conn = connection.lock().await;
        if let Err(e) = handle_client_message(&mut conn, message).await {
            error!("Error handling client message: {}", e);
        }
    }
}

/// Handles a single outgoing client message.
async fn handle_client_message<S>(
    connection: &mut RpcConnection<S>,
    message: ClientMessage,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match message {
        ClientMessage::Request {
            method,
            params,
            response_sender,
        } => {
            connection
                .send_request(method, params, response_sender)
                .await?;
        }
        ClientMessage::Notification { method, params } => {
            connection.send_notification(method, params).await?;
        }
    }
    Ok(())
}

/// A trait for creating connections.
///
/// `ConnectionMaker` provides a generic way to create objects that implement the `Connection` trait.
/// It is automatically implemented for any type that implements both `Connection` and `Default`.
///
/// The ConnectionMaker is used to create a new Connection object for each incoming connection.
pub trait ConnectionMaker<T>: Send + Sync
where
    T: Connection,
{
    /// Creates a new connection instance.
    fn make_connection(&self) -> T;
}

/// A [`ConnectionMaker`] implementation using a closure.
pub struct ConnectionMakerFn<F, T>
where
    F: FnMut() -> T + Send + Sync,
    T: Connection,
{
    /// The closure that creates connections.
    make_fn: F,
    /// Phantom data for the connection type.
    _phantom: PhantomData<T>,
}

impl<F, T> ConnectionMakerFn<F, T>
where
    F: Fn() -> T + Send + Sync,
    T: Connection,
{
    /// Creates a new `ConnectionMakerFn` from a closure.
    pub fn new(make_fn: F) -> Self {
        Self {
            make_fn,
            _phantom: PhantomData,
        }
    }
}

impl<F, T> ConnectionMaker<T> for ConnectionMakerFn<F, T>
where
    F: Fn() -> T + Send + Sync,
    T: Connection,
{
    fn make_connection(&self) -> T {
        (self.make_fn)()
    }
}

/// A single Connection in an RPC server or client. For server connections, a new instance of the
/// Connection is created for each incoming connection. For clients, a single instance is used for
/// the lifetime of the connection.
///
/// As a convencience for clients that don't need to handle requests or responses, the `Connection`
/// trait is implemented for `()`, and the `Client` type exposes `send_request` and
/// `send_notification` directly.
///
/// Use the `#[async_trait]` attribute from the `async_trait` crate when implementing this trait to
/// support async methods.
#[async_trait]
pub trait Connection: Send + Sync + 'static {
    /// Called after a connection is intiated, either by a `Client` connecting outbound, or an
    /// incoming connection on a listening `Server`.
    async fn connected(&self, _client: RpcSender) -> Result<()> {
        Ok(())
    }

    /// Handles an incoming RPC request.
    ///
    /// By default, returns an error indicating the method is not implemented.
    async fn handle_request(
        &self,
        _client: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value> {
        tracing::warn!("Unhandled request: method={}, params={:?}", method, params);
        Err(RpcError::Protocol(
            format!("Method '{}' not implemented", method).into(),
        ))
    }

    /// Handles an incoming RPC notification.
    ///
    /// By default, logs a warning about the unhandled notification.
    async fn handle_notification(
        &self,
        _client: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<()> {
        tracing::warn!(
            "Unhandled notification: method={}, params={:?}",
            method,
            params
        );
        Ok(())
    }
}

impl Connection for () {}

/// Low-level RPC connection handler for reading and writing messages over a stream.
#[derive(Debug)]
pub struct RpcConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Receiver for incoming messages.
    message_receiver: Option<mpsc::Receiver<Result<Message>>>,
    /// Write half of the stream.
    write_half: WriteHalf<S>,
    /// Next request ID to use.
    next_request_id: u32,
    /// Pending requests awaiting responses.
    pending_requests: HashMap<u32, oneshot::Sender<Result<Value>>>,
}

impl<S> RpcConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Creates a new RpcConnection with the given stream.
    pub fn new(stream: S) -> Self {
        let (read_half, write_half) = split(stream);
        let (message_sender, message_receiver) = mpsc::channel(1000);

        // Spawn a blocking task to read messages
        Handle::current().spawn_blocking(move || {
            let mut sync_reader = SyncIoBridge::new(read_half);
            loop {
                match Message::decode(&mut sync_reader) {
                    Ok(message) => match message_sender.blocking_send(Ok(message)) {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Error sending message: {}", e);
                            break;
                        }
                    },
                    Err(e) => {
                        // Receiver dropped means handler exited; ignore send errors.
                        drop(message_sender.blocking_send(Err(e)));
                        break;
                    }
                }
            }
        });

        Self {
            write_half,
            next_request_id: 1,
            pending_requests: HashMap::new(),
            message_receiver: Some(message_receiver),
        }
    }

    /// Takes ownership of the message receiver channel.
    pub fn receiver(&mut self) -> mpsc::Receiver<Result<Message>> {
        self.message_receiver
            .take()
            .expect("Receiver already taken")
    }

    /// Handles an incoming response message, routing it to the appropriate pending request.
    pub fn handle_response(&mut self, response: Response) -> Result<()> {
        if let Some(sender) = self.pending_requests.remove(&response.id) {
            // Receiver may be dropped if caller gave up waiting; ignore send errors.
            drop(sender.send(response.result.map_err(|e| {
                if let Value::Map(map) = e {
                    if let (Some(Value::String(name)), Some(value)) = (
                        map.iter()
                            .find(|(k, _)| k == &Value::from("name"))
                            .map(|(_, v)| v),
                        map.iter()
                            .find(|(k, _)| k == &Value::from("value"))
                            .map(|(_, v)| v),
                    ) {
                        RpcError::Service(ServiceError {
                            name: name.as_str().unwrap().to_string(),
                            value: value.clone(),
                        })
                    } else {
                        RpcError::Service(ServiceError {
                            name: "UnknownError".to_string(),
                            value: Value::Map(map),
                        })
                    }
                } else {
                    RpcError::Service(ServiceError {
                        name: "RemoteError".to_string(),
                        value: e,
                    })
                }
            })));
            Ok(())
        } else {
            Err(RpcError::Protocol(ProtocolError::UnexpectedResponse {
                id: response.id,
            }))
        }
    }

    /// Encodes and writes a message to the stream.
    pub async fn write_message(&mut self, message: &Message) -> Result<()> {
        trace!("sending message: {:?}", message);
        let mut buffer = Vec::new();
        message.encode(&mut buffer)?;
        self.write_half.write_all(&buffer).await?;
        self.write_half.flush().await?;
        Ok(())
    }

    /// Sends an RPC request and registers the response channel.
    pub async fn send_request(
        &mut self,
        method: String,
        params: Vec<Value>,
        response_sender: oneshot::Sender<Result<Value>>,
    ) -> Result<()> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.pending_requests.insert(id, response_sender);
        let request = Request { id, method, params };
        self.write_message(&Message::Request(request)).await
    }

    /// Sends an RPC notification (no response expected).
    pub async fn send_notification(&mut self, method: String, params: Vec<Value>) -> Result<()> {
        let notification = Notification { method, params };
        self.write_message(&Message::Notification(notification))
            .await
    }
}
