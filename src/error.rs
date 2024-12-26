use rmpv::Value;
use std::io;
use thiserror::Error;

/// Errors that can occur during RPC operations.
#[derive(Error, Debug)]
pub enum RpcError {
    /// Error occurred during I/O operations.
    #[error("I/O error: {0}")]
    Io(io::Error),

    /// Error occurred during MessagePack serialization.
    #[error("Serialization error: {0}")]
    Serialization(#[from] rmpv::encode::Error),

    /// Error occurred during MessagePack deserialization.
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] rmpv::decode::Error),

    /// Error related to the RPC protocol.
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Error returned by the RPC service implementation.
    #[error("Service error: {0}")]
    Service(ServiceError),

    #[error("Connection disconnected")]
    Disconnect,

    /// Error occurred while trying to establish a connection.
    #[error("Failed to connect to {0}")]
    Connect(String),
}

/// An error that occurred during the execution of an RPC service method.
///
/// It consists of a name, which identifies the type of error, and a value, which can contain
/// additional error details. This error type is used to convey service-specific errors back to the
/// client. When sent over the RPC protocol, this error will be serialized into a map with "name"
/// and "value" keys.
#[derive(Error, Debug)]
pub struct ServiceError {
    pub name: String,
    pub value: Value,
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Service error {}: {:?}", self.name, self.value)
    }
}

impl From<ServiceError> for Value {
    fn from(error: ServiceError) -> Self {
        Value::Map(vec![
            (
                Value::String("name".into()),
                Value::String(error.name.into()),
            ),
            (Value::String("value".into()), error.value),
        ])
    }
}

impl From<std::io::Error> for RpcError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            RpcError::Disconnect
        } else {
            RpcError::Io(error)
        }
    }
}

pub type Result<T> = std::result::Result<T, RpcError>;
