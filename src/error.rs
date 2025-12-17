use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    io::{self, ErrorKind},
    result,
};

use rmpv::{decode, encode, Value};
use thiserror::Error;

/// Errors that can occur during RPC operations.
#[derive(Error, Debug)]
pub enum RpcError {
    /// Error occurred during I/O operations.
    #[error("I/O error: {0}")]
    Io(io::Error),

    /// Error occurred during MessagePack serialization.
    #[error("Serialization error: {0}")]
    Serialization(#[from] encode::Error),

    /// Error occurred during MessagePack deserialization.
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] decode::Error),

    /// Error related to the RPC protocol.
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// Error returned by the RPC service implementation.
    #[error("Service error: {0}")]
    Service(ServiceError),

    /// The connection was closed.
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
    /// The error type name.
    pub name: String,
    /// Additional error data.
    pub value: Value,
}

impl Display for ServiceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        write!(f, "Service error {}: {:?}", self.name, self.value)
    }
}

impl From<ServiceError> for Value {
    fn from(error: ServiceError) -> Self {
        Self::Map(vec![
            (Self::String("name".into()), Self::String(error.name.into())),
            (Self::String("value".into()), error.value),
        ])
    }
}

impl From<io::Error> for RpcError {
    fn from(error: io::Error) -> Self {
        if error.kind() == ErrorKind::UnexpectedEof {
            Self::Disconnect
        } else {
            Self::Io(error)
        }
    }
}

/// A type alias for `Result` with [`RpcError`] as the error type.
pub type Result<T> = result::Result<T, RpcError>;
