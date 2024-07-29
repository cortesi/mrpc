//! Core RPC connection handling and message processing.
//!
//! Defines structures and traits for managing RPC connections,
//! handling incoming and outgoing messages, and implementing
//! RPC services.
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use async_trait::async_trait;
use rmpv::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    sync::{mpsc, oneshot},
};
use tracing::trace;

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

/// Manages bidirectional communication between a local service and a remote RPC connection.
pub(crate) struct ConnectionHandler<S, T: RpcService> {
    connection: RpcConnection<S>,
    service: Arc<T>,
    client_receiver: mpsc::Receiver<ClientMessage>,
    rpc_sender: RpcSender,
}

impl<S, T: RpcService> ConnectionHandler<S, T>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Creates a new ConnectionHandler with the given connection, service, and message channels.
    pub fn new(
        connection: RpcConnection<S>,
        service: Arc<T>,
        receiver: mpsc::Receiver<ClientMessage>,
        sender: mpsc::Sender<ClientMessage>,
    ) -> Self {
        Self {
            connection,
            service,
            client_receiver: receiver,
            rpc_sender: RpcSender { sender },
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                Some(client_message) = self.client_receiver.recv() => {
                    if let Err(e) = self.handle_client_message(client_message).await {
                        tracing::warn!("Error handling client message: {}", e);
                    }
                }
                message_result = self.connection.read_message() => {
                    match message_result {
                        Ok(message) => {
                            if let Err(e) = self.handle_incoming_message(message).await {
                                tracing::warn!("Error handling incoming message: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error reading message: {}", e);
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    async fn handle_incoming_message(&mut self, message: Message) -> Result<()> {
        match message {
            Message::Request(request) => {
                let result = self
                    .service
                    .handle_request::<S>(self.rpc_sender.clone(), &request.method, request.params)
                    .await;
                let response = match result {
                    Ok(value) => Response {
                        id: request.id,
                        result: Ok(value),
                    },
                    Err(RpcError::Service(service_error)) => {
                        tracing::warn!("Service error: {}", service_error);
                        Response {
                            id: request.id,
                            result: Err(service_error.into()),
                        }
                    }
                    Err(e) => {
                        tracing::warn!("RPC error: {}", e);
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
                    .handle_notification::<S>(
                        self.rpc_sender.clone(),
                        &notification.method,
                        notification.params,
                    )
                    .await
                {
                    tracing::warn!("Error handling notification: {}", e);
                }
            }
            Message::Response(response) => {
                if let Some(sender) = self.connection.pending_requests.remove(&response.id) {
                    let _ = sender.send(response.result.map_err(|e| {
                        RpcError::Service(ServiceError {
                            name: "RemoteError".to_string(),
                            value: e,
                        })
                    }));
                } else {
                    tracing::warn!("Received response for unknown request id: {}", response.id);
                }
            }
        }
        Ok(())
    }

    /// Processes a message from the local client API.
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
///
/// Example implementation:
/// ```
/// use mrpc::{Result, RpcHandle, RpcService};
/// use rmpv::Value;
///
/// #[derive(Clone)]
/// struct MyService;
///
/// #[async_trait::async_trait]
/// impl RpcService for MyService {
///     async fn handle_request<S>(
///         &self,
///         _client: RpcHandle,
///         method: &str,
///         params: Vec<Value>,
///     ) -> Result<Value> {
///         match method {
///             "greet" => {
///                 let name = params[0].as_str().unwrap_or("World");
///                 Ok(Value::String(format!("Hello, {}!", name).into()))
///             }
///             _ => Err(mrpc::RpcError::Protocol(format!("Unknown method: {}", method))),
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait RpcService: Send + Sync + Clone + 'static {
    /// Called after a connection is intiated, either by ai `Client` connecting outbound, or an
    /// incoming connection on a listening `Server`.
    async fn connected(&self, _client: RpcSender) {}

    /// Handles an incoming RPC request.
    ///
    /// By default, returns an error indicating the method is not implemented.
    async fn handle_request<S>(
        &self,
        _client: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tracing::warn!("Unhandled request: method={}, params={:?}", method, params);
        Err(RpcError::Protocol(format!(
            "Method '{}' not implemented",
            method
        )))
    }

    /// Handles an incoming RPC notification.
    ///
    /// By default, logs a warning about the unhandled notification.
    async fn handle_notification<S>(
        &self,
        _client: RpcSender,
        method: &str,
        params: Vec<Value>,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tracing::warn!(
            "Unhandled notification: method={}, params={:?}",
            method,
            params
        );
        Ok(())
    }
}

/// Low-level RPC connection handler for reading and writing messages over a stream.
#[derive(Debug)]
pub(crate) struct RpcConnection<S> {
    stream: S,
    next_request_id: u32,
    pending_requests: std::collections::HashMap<u32, oneshot::Sender<Result<Value>>>,
}

impl<S> RpcConnection<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Creates a new RpcConnection with the given stream.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            next_request_id: 1,
            pending_requests: std::collections::HashMap::new(),
        }
    }

    /// Reads and decodes the next message from the stream.
    pub async fn read_message(&mut self) -> Result<Message> {
        let mut length_bytes = [0u8; 4];
        self.stream.read_exact(&mut length_bytes).await?;
        let length = u32::from_be_bytes(length_bytes) as usize;

        let mut buffer = vec![0u8; length];
        self.stream.read_exact(&mut buffer).await?;

        let message = Message::decode(&mut &buffer[..])?;
        trace!("received message: {:?}", message);
        Ok(message)
    }

    /// Encodes and writes a message to the stream.
    pub async fn write_message(&mut self, message: &Message) -> Result<()> {
        trace!("sending message: {:?}", message);
        let mut buffer = Vec::new();
        message.encode(&mut buffer)?;

        let length = buffer.len() as u32;
        let length_bytes = length.to_be_bytes();

        self.stream.write_all(&length_bytes).await?;
        self.stream.write_all(&buffer).await?;
        self.stream.flush().await?;

        Ok(())
    }
}

impl<S> AsyncRead for RpcConnection<S>
where
    S: AsyncRead + Unpin,
{
    /// Polls the underlying stream for read readiness.
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl<S> AsyncWrite for RpcConnection<S>
where
    S: AsyncWrite + Unpin,
{
    /// Polls the underlying stream for write readiness.
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::result::Result<usize, std::io::Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    /// Flushes the underlying stream.
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    /// Closes the underlying stream.
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
