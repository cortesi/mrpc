//! Networking components for RPC server and client communication.
//!
//! Provides implementations for TCP and Unix domain socket transports,
//! as well as abstractions for RPC servers and clients.

use std::{
    fs,
    io::ErrorKind,
    marker::PhantomData,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
#[cfg(feature = "serde")]
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io,
    io::{AsyncRead, AsyncWrite, DuplexStream},
    net::{
        TcpListener as TokioTcpListener, TcpStream, UnixListener as TokioUnixListener, UnixStream,
    },
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};
use tracing::{trace, warn};

use crate::{
    Connection, ConnectionMaker, ConnectionMakerFn, RpcSender, Value,
    connection::{ConnectionHandler, RpcConnection},
    error::*,
};

/// Create a pair of connected in-memory streams.
pub fn duplex(buffer_size: usize) -> (DuplexStream, DuplexStream) {
    io::duplex(buffer_size)
}

/// A listener that yields bidirectional streams.
#[async_trait]
pub trait Listener: Send + Sync + 'static {
    /// The stream type produced by accepting a connection.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Accept the next connection.
    async fn accept(&self) -> Result<Self::Stream>;
}

/// A bidirectional stream usable by the server accept loop.
trait AsyncStream: AsyncRead + AsyncWrite {}

impl<T> AsyncStream for T where T: AsyncRead + AsyncWrite {}

/// A boxed bidirectional stream used to erase concrete stream types.
type BoxedStream = Box<dyn AsyncStream + Unpin + Send>;

/// Adapter that type-erases a [`Listener`] by boxing accepted streams.
struct ErasedListener<L> {
    /// The wrapped listener.
    inner: L,
}

#[async_trait]
impl<L> Listener for ErasedListener<L>
where
    L: Listener,
{
    type Stream = BoxedStream;

    async fn accept(&self) -> Result<Self::Stream> {
        Ok(Box::new(self.inner.accept().await?))
    }
}

/// TCP listener for accepting RPC connections.
struct TcpListener {
    /// The underlying tokio TCP listener.
    inner: TokioTcpListener,
}

impl TcpListener {
    /// Binds a TCP listener to the given address.
    pub async fn bind(addr: &str) -> Result<Self> {
        trace!("Binding TCP listener to address: {}", addr);
        let listener = TokioTcpListener::bind(addr).await?;
        Ok(Self { inner: listener })
    }
}

#[async_trait]
impl Listener for TcpListener {
    type Stream = TcpStream;

    async fn accept(&self) -> Result<Self::Stream> {
        let (stream, addr) = self.inner.accept().await?;
        trace!("Accepted TCP connection from: {}", addr);
        Ok(stream)
    }
}

/// Unix domain socket listener for accepting RPC connections.
struct UnixListener {
    /// The underlying tokio Unix listener.
    inner: TokioUnixListener,
    /// The path that was bound.
    path: PathBuf,
}

impl UnixListener {
    /// Binds a Unix listener to the given path.
    pub async fn bind<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy();
        trace!("Binding Unix listener to path: {}", path_str);
        let listener = TokioUnixListener::bind(&path)?;
        Ok(Self {
            inner: listener,
            path: path.as_ref().to_path_buf(),
        })
    }
}

#[async_trait]
impl Listener for UnixListener {
    type Stream = UnixStream;

    async fn accept(&self) -> Result<Self::Stream> {
        let (stream, _) = self.inner.accept().await?;
        trace!("Accepted Unix connection");
        Ok(stream)
    }
}

impl Drop for UnixListener {
    fn drop(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                warn!("Failed to remove unix socket at {:?}: {}", self.path, e);
            }
        }
    }
}

/// RPC server that can listen on TCP or Unix domain sockets. The service type must implement the
/// `RpcService` trait and `Default`. A new service instance is created for each connection.
pub struct Server<T>
where
    T: Connection,
{
    /// Factory for creating connection handlers.
    connection_maker: Arc<dyn ConnectionMaker<T> + Send + Sync>,
    /// The configured listener.
    listener: Option<Box<dyn Listener<Stream = BoxedStream>>>,
    /// The bound TCP address, when applicable.
    local_addr: Option<SocketAddr>,
    /// Phantom data for the connection type.
    _phantom: PhantomData<T>,
}

impl<T> Server<T>
where
    T: Connection,
{
    /// Creates a new Server with the given ConnectionMaker.
    pub fn from_maker<M>(maker: M) -> Self
    where
        M: ConnectionMaker<T> + Send + Sync + 'static,
    {
        Self {
            connection_maker: Arc::new(maker),
            listener: None,
            local_addr: None,
            _phantom: PhantomData,
        }
    }

    /// Helper method to create a Server from a closure
    pub fn from_fn<F>(f: F) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        Self::from_maker(ConnectionMakerFn::new(f))
    }

    /// Returns the bound address of the server. Only valid for TCP listeners that have already
    /// been bound, otherwise returns an error.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        if self.listener.is_none() {
            return Err(RpcError::Protocol("No listener configured".into()));
        }
        self.local_addr
            .ok_or_else(|| RpcError::Protocol("listener has no SocketAddr".into()))
    }

    /// Configures the server to listen on a TCP address.
    pub async fn tcp(mut self, addr: &str) -> Result<Self> {
        let tcp_listener = TcpListener::bind(addr).await?;
        self.local_addr = Some(tcp_listener.inner.local_addr()?);
        self.listener = Some(Box::new(ErasedListener {
            inner: tcp_listener,
        }));
        Ok(self)
    }

    /// Configures the server to listen on a Unix domain socket.
    pub async fn unix<P: AsRef<Path>>(mut self, path: P) -> Result<Self> {
        self.local_addr = None;
        self.listener = Some(Box::new(ErasedListener {
            inner: UnixListener::bind(path).await?,
        }));
        Ok(self)
    }

    /// Configures the server to accept connections from a custom listener.
    pub fn with_listener<L>(mut self, listener: L) -> Result<Self>
    where
        L: Listener,
    {
        self.local_addr = None;
        self.listener = Some(Box::new(ErasedListener { inner: listener }));
        Ok(self)
    }

    /// Starts the server and returns a handle for lifecycle control.
    pub async fn spawn(self) -> Result<ServerHandle> {
        let Self {
            connection_maker,
            listener,
            local_addr,
            _phantom,
        } = self;

        let listener =
            listener.ok_or_else(|| RpcError::Protocol("No listener configured".into()))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task =
            tokio::spawn(
                async move { run_listener(listener, connection_maker, shutdown_rx).await },
            );

        Ok(ServerHandle {
            shutdown_tx,
            task,
            local_addr,
        })
    }

    /// Starts the server and begins accepting connections.
    pub async fn run(self) -> Result<()> {
        self.spawn().await?.join().await
    }
}

impl<T> Server<T>
where
    T: Connection + Default,
{
    /// Creates a new server from a listener using `T::default` for each connection.
    pub fn from_listener<L>(listener: L) -> Result<Self>
    where
        L: Listener,
    {
        Self::from_fn(T::default).with_listener(listener)
    }
}

/// A handle for controlling a running server.
#[derive(Debug)]
pub struct ServerHandle {
    /// Used to signal shutdown to the accept loop.
    shutdown_tx: watch::Sender<bool>,
    /// Background task running the accept loop.
    task: JoinHandle<Result<()>>,
    /// The bound TCP address, when using a TCP listener.
    local_addr: Option<SocketAddr>,
}

impl ServerHandle {
    /// Signals the server to stop accepting new connections.
    pub fn shutdown(&self) {
        let _send_result = self.shutdown_tx.send(true);
    }

    /// Waits for the server to stop and returns any error.
    pub async fn join(self) -> Result<()> {
        self.task
            .await
            .map_err(|e| RpcError::Protocol(e.to_string().into()))?
    }

    /// Returns the bound TCP address of the server.
    ///
    /// This is only valid for TCP listeners; Unix listeners do not have a `SocketAddr`.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.local_addr
            .ok_or_else(|| RpcError::Protocol("listener has no SocketAddr".into()))
    }
}

/// Runs an accept loop until shutdown is signalled, cancelling active connection tasks on exit.
async fn run_listener<T>(
    listener: Box<dyn Listener<Stream = BoxedStream>>,
    connection_maker: Arc<dyn ConnectionMaker<T> + Send + Sync>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()>
where
    T: Connection,
{
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let stream = accepted?;
                let rpc_conn = RpcConnection::new(stream);
                let connection = connection_maker.make_connection();
                connections.spawn(async move {
                    serve_connection(rpc_conn, connection).await;
                });
            }
            Some(joined) = connections.join_next(), if !connections.is_empty() => {
                if let Err(e) = joined {
                    warn!("Error joining connection task: {}", e);
                }
            }
        }
    }

    connections.abort_all();
    while let Some(joined) = connections.join_next().await {
        if let Err(e) = joined {
            warn!("Error joining connection task: {}", e);
        }
    }

    Ok(())
}

/// Serves a single accepted connection until it disconnects.
async fn serve_connection<S, T>(rpc_conn: RpcConnection<S>, connection: T)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    T: Connection,
{
    let (sender, receiver) = mpsc::channel(100);
    let handler = ConnectionHandler::new(rpc_conn, connection, sender.clone());
    match handler.run(receiver).await {
        Ok(()) => {
            trace!("Connection handler finished successfully");
        }
        Err(RpcError::Disconnect { .. }) => {
            trace!("Client disconnected");
        }
        Err(e) => {
            warn!("Connection error: {}", e);
        }
    }
}

/// RPC client for connecting to a server over TCP or Unix domain sockets.
#[derive(Debug)]
pub struct Client<T: Connection> {
    /// Sender for sending RPC requests and notifications.
    pub sender: RpcSender,
    /// Handle to the background connection handler task.
    handle: Option<JoinHandle<()>>,
    /// Used to request shutdown of the background connection handler.
    shutdown_tx: watch::Sender<bool>,
    /// Phantom data for the connection type.
    _phantom: PhantomData<T>,
}

impl<T: Connection> Client<T> {
    /// Creates a new client from any bidirectional stream.
    pub async fn from_stream<S>(stream: S, service: T) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::new(RpcConnection::new(stream), service).await
    }

    /// Creates a new client connected to a Unix domain socket.
    pub async fn connect_unix<P: AsRef<Path>>(path: P, service: T) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        let stream = UnixStream::connect(path)
            .await
            .map_err(|source| RpcError::Connect { source })?;
        trace!("Unix connection established to: {:?}", path_str);
        Self::new(RpcConnection::new(stream), service).await
    }

    /// Creates a new client connected to a TCP address.
    pub async fn connect_tcp(addr: &str, service: T) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|source| RpcError::Connect { source })?;
        trace!("TCP connection established to: {}", addr);
        Self::new(RpcConnection::new(stream), service).await
    }

    /// Creates a new client from an existing RPC connection.
    async fn new<S>(connection: RpcConnection<S>, service: T) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let shutdown_tx = connection.shutdown_sender();
        let (sender, receiver) = mpsc::channel(100);
        let rpc_sender = RpcSender {
            sender: sender.clone(),
        };
        let handler = ConnectionHandler::new(connection, service, sender);
        let handler_task = tokio::spawn(async move {
            if let Err(e) = handler.run(receiver).await {
                match e {
                    RpcError::Disconnect { .. } => {
                        tracing::trace!("Client disconnected");
                    }
                    e => {
                        tracing::warn!("Handler error: {}", e);
                    }
                }
            }
        });

        Ok(Self {
            sender: rpc_sender,
            handle: Some(handler_task),
            shutdown_tx,
            _phantom: PhantomData,
        })
    }

    /// Sends an RPC request to the server. Convenience method for `RpcSender::send_request`.
    pub async fn send_request(&self, method: &str, params: &[Value]) -> Result<Value> {
        self.sender.send_request(method, params).await
    }

    /// Sends an RPC notification to the server. Convenience method for
    /// `RpcSender::send_notification`.
    pub async fn send_notification(&self, method: &str, params: &[Value]) -> Result<()> {
        self.sender.send_notification(method, params).await
    }

    /// Sends a typed request and deserializes the response.
    #[cfg(feature = "serde")]
    pub async fn call<Req, Resp>(&self, method: &str, req: &Req) -> Result<Resp>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        self.sender.call(method, req).await
    }

    /// Sends a typed notification.
    #[cfg(feature = "serde")]
    pub async fn notify<Req>(&self, method: &str, req: &Req) -> Result<()>
    where
        Req: Serialize,
    {
        self.sender.notify(method, req).await
    }

    /// Waits for the client handler task to complete.
    pub async fn join(mut self) -> Result<()> {
        let handle = self
            .handle
            .take()
            .expect("Client join called with no handler task");
        handle
            .await
            .map_err(|e| RpcError::Protocol(e.to_string().into()))?;
        Ok(())
    }

    /// Signals the client to stop processing messages and close the connection.
    pub fn shutdown(&self) {
        let _send_result = self.shutdown_tx.send(true);
    }

    /// Shuts down the client and waits for the handler task to complete.
    pub async fn close(self) -> Result<()> {
        self.shutdown();
        self.join().await
    }
}

impl<T: Connection> Drop for Client<T> {
    fn drop(&mut self) {
        let _send_result = self.shutdown_tx.send(true);
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}
