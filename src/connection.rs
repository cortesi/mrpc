//! Core RPC connection handling and message processing.
//!
//! Defines structures and traits for managing RPC connections,
//! handling incoming and outgoing messages, and implementing
//! RPC services.
use std::{
    collections::HashMap,
    future::Future,
    io::{Cursor, ErrorKind},
    marker::PhantomData,
    pin::Pin,
    result,
    sync::Arc,
    task::{Context, Poll},
};

use async_trait::async_trait;
use bytes::{Buf, BytesMut};
use rmpv::{Value, decode};
#[cfg(feature = "serde")]
use rmpv::{decode::read_value, encode::write_value};
#[cfg(feature = "serde")]
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, WriteHalf, split},
    sync::{Mutex, mpsc, oneshot, watch},
    task::{JoinError, JoinHandle, JoinSet},
};
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

/// Wraps a [`JoinHandle`] and aborts it on drop.
///
/// This keeps spawned tasks from leaking if an async function is cancelled while the task is still
/// running.
struct AbortOnDrop<T> {
    /// The wrapped task handle.
    handle: JoinHandle<T>,
}

impl<T> AbortOnDrop<T> {
    /// Wrap a task handle that should be aborted when dropped.
    fn new(handle: JoinHandle<T>) -> Self {
        Self { handle }
    }

    /// Abort the wrapped task.
    fn abort(&self) {
        self.handle.abort();
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.abort();
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = result::Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        Pin::new(&mut this.handle).poll(cx)
    }
}

#[cfg(feature = "serde")]
/// Serializes a typed value into a MessagePack value.
pub fn serialize_value<T>(value: &T) -> Result<Value>
where
    T: Serialize,
{
    let buf = rmp_serde::to_vec_named(value)?;
    Ok(read_value(&mut Cursor::new(buf))?)
}

#[cfg(feature = "serde")]
/// Serializes a typed request into a MessagePack-RPC params array.
///
/// If the encoded value is an array, its elements become the params array. Otherwise, the encoded
/// value is sent as a single parameter.
pub fn serialize_params<Req>(req: &Req) -> Result<Vec<Value>>
where
    Req: Serialize,
{
    let value = serialize_value(req)?;
    match value {
        Value::Array(values) => Ok(values),
        value => Ok(vec![value]),
    }
}

#[cfg(feature = "serde")]
/// Deserializes a typed response from a MessagePack value.
pub fn deserialize_response<Resp>(value: &Value) -> Result<Resp>
where
    Resp: DeserializeOwned,
{
    let mut buf = Vec::new();
    write_value(&mut buf, value)?;
    Ok(rmp_serde::from_slice(&buf)?)
}

#[cfg(feature = "serde")]
/// Deserializes a typed request from a MessagePack-RPC params list.
///
/// This is intended for servers implementing [`Connection::handle_request`] or
/// [`Connection::handle_notification`], where incoming parameters are provided as a `Vec<Value>`.
pub fn deserialize_params<Req>(params: Vec<Value>) -> Result<Req>
where
    Req: DeserializeOwned,
{
    let value = Value::Array(params);
    deserialize_response(&value)
}

#[cfg(feature = "serde")]
/// Deserializes a typed request from a single MessagePack-RPC parameter.
///
/// This is intended for servers implementing [`Connection::handle_request`] or
/// [`Connection::handle_notification`], where incoming parameters are provided as a `Vec<Value>`.
pub fn deserialize_param<Req>(params: Vec<Value>) -> Result<Req>
where
    Req: DeserializeOwned,
{
    let mut values = params.into_iter();
    let value = values
        .next()
        .ok_or_else(|| RpcError::Protocol("Expected exactly one parameter".into()))?;
    if values.next().is_some() {
        return Err(RpcError::Protocol("Expected exactly one parameter".into()));
    }
    deserialize_response(&value)
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
    pub(crate) fn new(
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
    pub(crate) async fn run(&self, client_receiver: mpsc::Receiver<ClientMessage>) -> Result<()> {
        let rpc_sender_clone = self.rpc_sender.clone();

        // Run the connected handler concurrently so it can send messages immediately.
        let service = Arc::clone(&self.service);
        let mut connected_task = AbortOnDrop::new(tokio::spawn(async move {
            service.connected(rpc_sender_clone).await
        }));

        let mut connected_done = false;
        let mut receiver = {
            let mut conn = self.connection.lock().await;
            conn.receiver()
        };

        // Clone Arc<Mutex<RpcConnection>> for the client message handling task
        let connection_clone = self.connection.clone();

        // Spawn a task to handle client messages
        let client_handler = AbortOnDrop::new(tokio::spawn(async move {
            handle_client_messages(connection_clone, client_receiver).await
        }));

        let mut incoming_handlers: JoinSet<()> = JoinSet::new();

        loop {
            tokio::select! {
                message_result = receiver.recv() => {
                    match message_result {
                        Some(Ok(message)) => {
                            let connection = self.connection.clone();
                            let service = Arc::clone(&self.service);
                            let rpc_sender = self.rpc_sender.clone();
                            incoming_handlers.spawn(async move {
                                if let Err(e) = handle_incoming_message(connection, service, rpc_sender, message).await {
                                    error!("Error handling incoming message: {}", e);
                                }
                            });
                        }
                        Some(Err(e)) => return Err(e),
                        None => break,
                    }
                }
                connected_result = &mut connected_task, if !connected_done => {
                    connected_done = true;
                    match connected_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => return Err(e),
                        Err(_) => {
                            return Err(RpcError::Protocol("Connected task failed".into()));
                        }
                    }
                }
                Some(joined) = incoming_handlers.join_next(), if !incoming_handlers.is_empty() => {
                    if let Err(e) = joined
                        && !e.is_cancelled() {
                            error!("Error joining incoming message handler: {}", e);
                        }
                }
                else => {
                    break;
                }
            }
        }

        connected_task.abort();
        client_handler.abort();
        incoming_handlers.abort_all();

        while let Some(joined) = incoming_handlers.join_next().await {
            if let Err(e) = joined
                && !e.is_cancelled()
            {
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
    /// Used to request shutdown of the background reader task.
    shutdown_tx: watch::Sender<bool>,
    /// Background task reading and decoding incoming RPC messages.
    read_task: JoinHandle<()>,
}

impl<S> RpcConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Creates a new RpcConnection with the given stream.
    pub fn new(stream: S) -> Self {
        let (read_half, write_half) = split(stream);
        let (message_sender, message_receiver) = mpsc::channel(1000);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let read_task = tokio::spawn(async move {
            let mut read_half = read_half;
            let mut buffer = BytesMut::with_capacity(8192);
            let mut eof = false;

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }

                match try_decode_message(&buffer) {
                    Ok(Some((message, consumed))) => {
                        buffer.advance(consumed);
                        if message_sender.send(Ok(message)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) if eof => {
                        // Receiver dropped means handler exited; ignore send errors.
                        drop(
                            message_sender
                                .send(Err(RpcError::Disconnect { source: None }))
                                .await,
                        );
                        break;
                    }
                    Ok(None) => {
                        let read_result = tokio::select! {
                            _ = shutdown_rx.changed() => {
                                continue;
                            }
                            read_result = read_half.read_buf(&mut buffer) => {
                                read_result
                            }
                        };
                        match read_result {
                            Ok(0) => {
                                eof = true;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                // Receiver dropped means handler exited; ignore send errors.
                                drop(message_sender.send(Err(RpcError::from(e))).await);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        // Receiver dropped means handler exited; ignore send errors.
                        drop(message_sender.send(Err(e)).await);
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
            shutdown_tx,
            read_task,
        }
    }

    /// Returns a sender used to request shutdown of the background reader task.
    pub(crate) fn shutdown_sender(&self) -> watch::Sender<bool> {
        self.shutdown_tx.clone()
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
            drop(
                sender.send(
                    response
                        .result
                        .map_err(|e| match ServiceError::try_from(e) {
                            Ok(service_error) => RpcError::Service(service_error),
                            Err(Value::Map(map)) => RpcError::Service(ServiceError {
                                name: "UnknownError".to_string(),
                                value: Value::Map(map),
                            }),
                            Err(original_value) => RpcError::Service(ServiceError {
                                name: "RemoteError".to_string(),
                                value: original_value,
                            }),
                        }),
                ),
            );
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

impl<S> Drop for RpcConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn drop(&mut self) {
        self.read_task.abort();
    }
}

/// Attempts to decode a single message from the beginning of `buffer`.
///
/// Returns `Ok(None)` when `buffer` doesn't contain a full MessagePack value yet.
fn try_decode_message(buffer: &[u8]) -> Result<Option<(Message, usize)>> {
    let mut cursor = Cursor::new(buffer);

    match decode::read_value(&mut cursor) {
        Ok(value) => {
            let consumed = cursor.position() as usize;
            let message = Message::from_value(value)?;
            Ok(Some((message, consumed)))
        }
        Err(decode::Error::InvalidMarkerRead(e) | decode::Error::InvalidDataRead(e))
            if e.kind() == ErrorKind::UnexpectedEof =>
        {
            Ok(None)
        }
        Err(decode::Error::DepthLimitExceeded) => {
            Err(RpcError::Protocol("Depth limit exceeded".into()))
        }
        Err(e) => Err(RpcError::Deserialization(e)),
    }
}
