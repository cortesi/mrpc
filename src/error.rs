use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    io::{self, ErrorKind},
    result,
};

#[cfg(feature = "serde")]
use rmp_serde::{decode::Error as RmpSerdeDecodeError, encode::Error as RmpSerdeEncodeError};
use rmpv::{Value, decode::Error as RmpvDecodeError, encode::Error as RmpvEncodeError};
use thiserror::Error;

/// Errors indicating a violation of the MessagePack-RPC protocol or message framing.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Received a message with an invalid type tag.
    #[error("Invalid message type: {0}")]
    InvalidMessageType(u64),

    /// Received a response for a request id that has no pending waiter.
    #[error("Unexpected response id: {id}")]
    UnexpectedResponse {
        /// The request id.
        id: u32,
    },

    /// Received a message that does not match the expected structure.
    #[error("Malformed message: {0}")]
    MalformedMessage(String),
}

impl From<&str> for ProtocolError {
    fn from(message: &str) -> Self {
        Self::MalformedMessage(message.to_string())
    }
}

impl From<String> for ProtocolError {
    fn from(message: String) -> Self {
        Self::MalformedMessage(message)
    }
}

/// Errors that can occur during RPC operations.
#[derive(Error, Debug)]
pub enum RpcError {
    /// Error occurred during I/O operations.
    #[error("I/O error: {0}")]
    Io(io::Error),

    /// Error occurred while trying to establish a connection.
    #[error("Connection failed")]
    Connect {
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Error occurred during MessagePack serialization.
    #[error("Serialization error: {0}")]
    Serialization(#[from] RmpvEncodeError),

    /// Error occurred during MessagePack deserialization.
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] RmpvDecodeError),

    /// Failed to serialize request parameters.
    #[cfg(feature = "serde")]
    #[error("Request serialization error: {0}")]
    RequestSerialization(#[from] RmpSerdeEncodeError),

    /// Failed to deserialize a response body.
    #[cfg(feature = "serde")]
    #[error("Response deserialization error: {0}")]
    ResponseDeserialization(#[from] RmpSerdeDecodeError),

    /// Error related to the MessagePack-RPC protocol.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// Error returned by the RPC service implementation.
    #[error("Service error: {0}")]
    Service(ServiceError),

    /// The connection was closed.
    #[error("Connection disconnected")]
    Disconnect {
        /// Underlying I/O error, when available.
        #[source]
        source: Option<io::Error>,
    },
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
        match error.kind() {
            ErrorKind::UnexpectedEof
            | ErrorKind::BrokenPipe
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::NotConnected => Self::Disconnect {
                source: Some(error),
            },
            _ => Self::Io(error),
        }
    }
}

/// A type alias for `Result` with [`RpcError`] as the error type.
pub type Result<T> = result::Result<T, RpcError>;
