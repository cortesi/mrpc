//! Networking components for RPC server and client communication.
//!
//! Provides implementations for TCP and Unix domain socket transports,
//! as well as abstractions for RPC servers and clients.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use rmpv::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{
        TcpListener as TokioTcpListener, TcpStream, UnixListener as TokioUnixListener, UnixStream,
    },
    sync::mpsc,
};
use tracing::trace;

use crate::error::*;
use crate::{ConnectionHandler, RpcConnection, RpcHandle, RpcService};

/// The interface for sending RPC requests and notifications.
///
/// This trait is implemented by types that can send RPC messages to a remote service.
/// Users typically don't need to implement this trait directly, as it's provided
/// by the library's client implementations.
///
/// Use this trait when you need to abstract over different RPC sender implementations
/// or when writing generic code that interacts with RPC services.
///
/// Example usage:
/// ```
/// use mrpc::{RpcSender, Result};
/// use rmpv::Value;
///
/// async fn greet_user<T: RpcSender>(sender: &T, name: &str) -> Result<String> {
///     let response = sender.send_request(
///         "greet".to_string(),
///         vec![Value::String(name.into())]
///     ).await?;
///     
///     Ok(response.as_str().unwrap_or("").to_string())
/// }
///
/// // Later, with a client:
/// // let client = Client::connect_tcp("127.0.0.1:8080", MyService).await?;
/// // let greeting = greet_user(&client, "Alice").await?;
/// // println!("{}", greeting);
/// ```
#[async_trait]
pub trait RpcSender {
    async fn send_request(&self, method: &str, params: Vec<Value>) -> Result<Value>;
    async fn send_notification(&self, method: &str, params: Vec<Value>) -> Result<()>;
}

/// TCP listener for accepting RPC connections.
struct TcpListener {
    inner: TokioTcpListener,
}

impl TcpListener {
    pub async fn bind(addr: &str) -> Result<Self> {
        trace!("Binding TCP listener to address: {}", addr);
        let listener = TokioTcpListener::bind(addr).await?;
        Ok(Self { inner: listener })
    }

    async fn accept(&self) -> Result<RpcConnection<TcpStream>> {
        let (stream, addr) = self.inner.accept().await?;
        trace!("Accepted TCP connection from: {}", addr);
        Ok(RpcConnection::new(stream))
    }
}

/// Unix domain socket listener for accepting RPC connections.
struct UnixListener {
    inner: TokioUnixListener,
}

impl UnixListener {
    pub async fn bind<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy();
        trace!("Binding Unix listener to path: {}", path_str);
        let listener = TokioUnixListener::bind(path)?;
        Ok(Self { inner: listener })
    }

    async fn accept(&self) -> Result<RpcConnection<UnixStream>> {
        let (stream, _) = self.inner.accept().await?;
        trace!("Accepted Unix connection");
        Ok(RpcConnection::new(stream))
    }
}

/// Trait for types that can accept incoming RPC connections.
#[async_trait]
trait Accept {
    type Stream: AsyncRead + AsyncWrite + Unpin;
    async fn accept(&self) -> Result<RpcConnection<Self::Stream>>;
}

#[async_trait]
impl Accept for TcpListener {
    type Stream = TcpStream;
    async fn accept(&self) -> Result<RpcConnection<Self::Stream>> {
        self.accept().await
    }
}

#[async_trait]
impl Accept for UnixListener {
    type Stream = UnixStream;
    async fn accept(&self) -> Result<RpcConnection<Self::Stream>> {
        self.accept().await
    }
}

/// Either a TCP or Unix domain socket listener.
enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

/// RPC server that can listen on TCP or Unix domain sockets.
pub struct Server<T: RpcService> {
    service: Arc<T>,
    listener: Option<Listener>,
}

impl<T: RpcService> Server<T> {
    /// Creates a new server with the given RPC service.
    pub fn new(service: T) -> Self {
        Self {
            service: Arc::new(service),
            listener: None,
        }
    }

    /// Configures the server to listen on a TCP address.
    pub async fn tcp(mut self, addr: &str) -> Result<Self> {
        self.listener = Some(Listener::Tcp(TcpListener::bind(addr).await?));
        Ok(self)
    }

    /// Configures the server to listen on a Unix domain socket.
    pub async fn unix<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        self.listener = Some(Listener::Unix(UnixListener::bind(path).await?));
        Ok(self)
    }

    /// Starts the server and begins accepting connections.
    pub async fn run(self) -> Result<()> {
        let listener = self
            .listener
            .ok_or_else(|| RpcError::Protocol("No listener configured".into()))?;
        match listener {
            Listener::Tcp(tcp_listener) => Self::run_internal(self.service, tcp_listener).await,
            Listener::Unix(unix_listener) => Self::run_internal(self.service, unix_listener).await,
        }
    }

    async fn run_internal<L>(service: Arc<T>, listener: L) -> Result<()>
    where
        L: Accept,
        L::Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        loop {
            let connection = RpcConnection::new(listener.accept().await?);
            let service_clone = service.clone();

            tokio::spawn(async move {
                let (sender, receiver) = mpsc::channel(100);
                let rpc_handle = RpcHandle {
                    sender: sender.clone(),
                };
                let mut handler = ConnectionHandler::new(
                    connection,
                    service_clone.clone(),
                    receiver,
                    sender.clone(),
                );
                service_clone.connected(rpc_handle).await;
                if let Err(e) = handler.run().await {
                    tracing::error!("Connection error: {}", e);
                }
            });
        }
    }
}

/// RPC client for connecting to a server over TCP or Unix domain sockets.
#[derive(Clone, Debug)]
pub struct Client<T: RpcService> {
    sender: RpcHandle,
    _handler: Arc<tokio::task::JoinHandle<()>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: RpcService> Client<T> {
    /// Creates a new client connected to a Unix domain socket.
    pub async fn connect_unix<P: AsRef<Path>>(path: P, service: T) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let stream = UnixStream::connect(path).await?;
        trace!("Unix connection established to: {:?}", path_str);
        Self::new(RpcConnection::new(stream), service).await
    }

    /// Creates a new client connected to a TCP address.
    pub async fn connect_tcp(addr: &str, service: T) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        trace!("TCP connection established to: {}", addr);
        Self::new(RpcConnection::new(stream), service).await
    }

    async fn new<S>(connection: RpcConnection<S>, service: T) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel(100);
        let rpc_sender = RpcHandle {
            sender: sender.clone(),
        };
        let service = Arc::new(service);
        let mut handler = ConnectionHandler::new(connection, service.clone(), receiver, sender);
        service.connected(rpc_sender.clone()).await;
        let handler_task = tokio::spawn(async move {
            if let Err(e) = handler.run().await {
                tracing::error!("Handler error: {}", e);
            }
        });

        Ok(Self {
            sender: rpc_sender,
            _handler: Arc::new(handler_task),
            _phantom: std::marker::PhantomData,
        })
    }
}

#[async_trait]
impl<T: RpcService> RpcSender for Client<T> {
    async fn send_request(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        self.sender.send_request(method, params).await
    }

    async fn send_notification(&self, method: &str, params: Vec<Value>) -> Result<()> {
        self.sender.send_notification(method, params).await
    }
}
