//! Core RPC connection handling and message processing.
//!
//! Defines structures and traits for managing RPC connections,
//! handling incoming and outgoing messages, and implementing
//! RPC services.
use std::marker::PhantomData;

use async_trait::async_trait;
use rmpv::Value;
use tokio::runtime::Handle;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, WriteHalf},
    sync::{mpsc, oneshot},
};
use tokio_util::io::SyncIoBridge;

use tracing::{error, trace, warn};

use crate::{
    error::{Result, RpcError, ServiceError},
    message::*,
};

/// Internal message type for communication between the client API and the connection handler.
#[derive(Debug)]
pub(crate) enum ClientMessage {
    Request {
        method: String,
        params: Vec<Value>,
        response_sender: oneshot::Sender<Result<Value>>,
    },
    Notification {
        method: String,
        params: Vec<Value>,
    },
}

/// The interface for sending RPC requests and notifications.
#[derive(Debug, Clone)]
pub struct RpcSender {
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
            .map_err(|_| RpcError::Protocol("Failed to send request".to_string()))?;
        response_receiver
            .await
            .map_err(|_| RpcError::Protocol("Failed to receive response".to_string()))?
    }

    /// Sends an RPC notification without waiting for a response.
    pub async fn send_notification(&self, method: &str, params: &[Value]) -> Result<()> {
        self.sender
            .send(ClientMessage::Notification {
                method: method.to_string(),
                params: params.to_vec(),
            })
            .await
            .map_err(|_| RpcError::Protocol("Failed to send notification".to_string()))
    }
}

pub(crate) struct ConnectionHandler<S, T: Connection>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    connection: RpcConnection<S>,
    service: T,
    client_receiver: mpsc::Receiver<ClientMessage>,
    rpc_sender: RpcSender,
}

impl<S, T: Connection> ConnectionHandler<S, T>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(
        connection: RpcConnection<S>,
        service: T,
        client_receiver: mpsc::Receiver<ClientMessage>,
        client_sender: mpsc::Sender<ClientMessage>,
    ) -> Self {
        Self {
            connection,
            service,
            client_receiver,
            rpc_sender: RpcSender {
                sender: client_sender,
            },
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let (connected_tx, mut connected_rx) = tokio::sync::oneshot::channel();
        let rpc_sender_clone = self.rpc_sender.clone();

        // Spawn the connected method in a separate task
        let mut service_clone = self.service.clone();
        tokio::spawn(async move {
            let result = service_clone.connected(rpc_sender_clone).await;
            let _ = connected_tx.send(result);
        });

        let mut connected_done = false;

        loop {
            tokio::select! {
                Some(client_message) = self.client_receiver.recv() => {
                    if let Err(e) = self.handle_client_message(client_message).await {
                        error!("Error handling client message: {}", e);
                    }
                }
                Some(message_result) = self.connection.message_receiver.recv() => {
                    match message_result {
                        Ok(message) => {
                            if let Err(e) = self.handle_incoming_message(message).await {
                                error!("Error handling incoming message: {}", e);
                                if matches!(e, RpcError::Disconnect) {
                                    return Err(e);
                                }
                            }
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

        Ok(())
    }

    async fn handle_incoming_message(&mut self, message: Message) -> Result<()> {
        match message {
            Message::Request(request) => {
                let result = self
                    .service
                    .handle_request(self.rpc_sender.clone(), &request.method, request.params)
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
                self.connection
                    .write_message(&Message::Response(response))
                    .await?;
            }
            Message::Notification(notification) => {
                if let Err(e) = self
                    .service
                    .handle_notification(
                        self.rpc_sender.clone(),
                        &notification.method,
                        notification.params,
                    )
                    .await
                {
                    warn!("Error handling notification: {}", e);
                }
            }
            Message::Response(response) => {
                if let Some(sender) = self.connection.pending_requests.remove(&response.id) {
                    let _ = sender.send(response.result.map_err(|e| {
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
                    }));
                } else {
                    warn!("Received response for unknown request id: {}", response.id);
                }
            }
        }
        Ok(())
    }

    async fn handle_client_message(&mut self, message: ClientMessage) -> Result<()> {
        match message {
            ClientMessage::Request {
                method,
                params,
                response_sender,
            } => {
                let id = self.connection.next_request_id;
                self.connection.next_request_id += 1;
                self.connection.pending_requests.insert(id, response_sender);
                let request = Request { id, method, params };
                self.connection
                    .write_message(&Message::Request(request))
                    .await?;
            }
            ClientMessage::Notification { method, params } => {
                let notification = Notification { method, params };
                self.connection
                    .write_message(&Message::Notification(notification))
                    .await?;
            }
        }
        Ok(())
    }
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
    fn make_connection(&self) -> T;
}

// ClosureConnectionMaker implementation (assuming it's already defined)
pub struct ClosureConnectionMaker<F, T>
where
    F: Fn() -> T + Send + Sync,
    T: Connection,
{
    make_fn: F,
    _phantom: PhantomData<T>,
}

impl<F, T> ClosureConnectionMaker<F, T>
where
    F: Fn() -> T + Send + Sync,
    T: Connection,
{
    pub fn new(make_fn: F) -> Self {
        ClosureConnectionMaker {
            make_fn,
            _phantom: PhantomData,
        }
    }
}

impl<F, T> ConnectionMaker<T> for ClosureConnectionMaker<F, T>
where
    F: Fn() -> T + Send + Sync,
    T: Connection,
{
    fn make_connection(&self) -> T {
        (self.make_fn)()
    }
}

/// The interface for implementing RPC service functionality.
///
/// This trait allows you to create custom RPC services by implementing
/// methods to handle requests, notifications, and connection events.
///
/// Implementations of this trait can be used with the `Server` and `Client`
/// types to create RPC servers and clients.
///
/// Use the `#[async_trait]` attribute from the `async_trait` crate when
/// implementing this trait to support async methods.
#[async_trait]
pub trait Connection: Send + Sync + Clone + 'static {
    /// Called after a connection is intiated, either by ai `Client` connecting outbound, or an
    /// incoming connection on a listening `Server`.
    async fn connected(&mut self, _client: RpcSender) -> Result<()> {
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
        Err(RpcError::Protocol(format!(
            "Method '{}' not implemented",
            method
        )))
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

/// Low-level RPC connection handler for reading and writing messages over a stream.
pub(crate) struct RpcConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub(crate) write_half: WriteHalf<S>,
    pub(crate) next_request_id: u32,
    pub(crate) pending_requests: std::collections::HashMap<u32, oneshot::Sender<Result<Value>>>,
    pub(crate) message_receiver: mpsc::Receiver<Result<Message>>,
}

impl<S> RpcConnection<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    /// Creates a new RpcConnection with the given stream.
    pub fn new(stream: S) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
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
                        let _ = message_sender.blocking_send(Err(e));
                        break;
                    }
                }
            }
        });

        Self {
            write_half,
            next_request_id: 1,
            pending_requests: std::collections::HashMap::new(),
            message_receiver,
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
}
