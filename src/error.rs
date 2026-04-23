use std::{
    fmt::{Display, Formatter, Result as FmtResult},
    io::{self, ErrorKind},
    result,
};

#[cfg(feature = "serde")]
use rmp_serde::{decode::Error as RmpSerdeDecodeError, encode::Error as RmpSerdeEncodeError};
use rmpv::{Value, decode::Error as RmpvDecodeError, encode::Error as RmpvEncodeError};
use thiserror::Error;
use tokio::task::JoinError;

/// Errors indicating a violation of the MessagePack-RPC protocol or message framing.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Received a message with an invalid type tag.
    #[error("Invalid message type: {0}")]
    InvalidMessageType(u64),

    /// A full MessagePack value exceeded the configured nesting limit.
    #[error("Depth limit exceeded")]
    DepthLimitExceeded,

    /// A caller requested a bound socket address before any listener was configured.
    #[error("No listener configured")]
    ListenerNotConfigured,

    /// A configured listener does not expose a bound `SocketAddr`.
    #[error("Listener has no SocketAddr")]
    MissingSocketAddr,

    /// A MessagePack-RPC single-parameter helper received the wrong arity.
    #[error("Expected exactly one parameter")]
    ExpectedSingleParameter,

    /// A single-consumer resource was taken more than once.
    #[error("Resource already taken: {resource}")]
    ResourceAlreadyTaken {
        /// The resource that was already taken.
        resource: &'static str,
    },

    /// A background task failed before completing normally.
    #[error("Task '{task}' failed: {source}")]
    TaskFailed {
        /// The task that failed.
        task: &'static str,
        /// The underlying join failure.
        #[source]
        source: JoinError,
    },

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

impl TryFrom<Value> for ServiceError {
    type Error = Value;

    fn try_from(value: Value) -> result::Result<Self, Self::Error> {
        if let Value::Map(map) = &value {
            let mut name = None;
            let mut service_value = None;

            for (key, entry) in map {
                match key.as_str() {
                    Some("name") => {
                        name = entry.as_str().map(ToOwned::to_owned);
                    }
                    Some("value") => {
                        service_value = Some(entry.clone());
                    }
                    _ => {}
                }
            }

            if let (Some(name), Some(service_value)) = (name, service_value) {
                return Ok(Self {
                    name,
                    value: service_value,
                });
            }
        }
        Err(value)
    }
}

impl RpcError {
    /// Wraps a failed task join as a protocol error with task context.
    pub(crate) fn task_failed(task: &'static str, source: JoinError) -> Self {
        Self::Protocol(ProtocolError::TaskFailed { task, source })
    }

    /// Reports that a single-consumer resource was taken more than once.
    pub(crate) fn resource_already_taken(resource: &'static str) -> Self {
        Self::Protocol(ProtocolError::ResourceAlreadyTaken { resource })
    }

    /// Builds a service-facing RPC error from a remote error payload.
    ///
    /// Properly encoded service errors preserve their original name and value.
    /// Malformed remote errors are wrapped into fallback names so callers still
    /// receive the remote payload through the service-error path.
    pub(crate) fn from_remote_error_value(value: Value) -> Self {
        match ServiceError::try_from(value) {
            Ok(service_error) => Self::Service(service_error),
            Err(Value::Map(map)) => Self::Service(ServiceError {
                name: "UnknownError".to_string(),
                value: Value::Map(map),
            }),
            Err(original_value) => Self::Service(ServiceError {
                name: "RemoteError".to_string(),
                value: original_value,
            }),
        }
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

#[cfg(test)]
mod tests {
    use futures::future::pending;

    use super::*;

    #[tokio::test]
    async fn test_task_failed_wraps_join_error() {
        let handle = tokio::spawn(async {
            pending::<()>().await;
        });
        handle.abort();
        let join_error = handle.await.unwrap_err();

        let error = RpcError::task_failed("demo task", join_error);

        match error {
            RpcError::Protocol(ProtocolError::TaskFailed { task, source }) => {
                assert_eq!(task, "demo task");
                assert!(source.is_cancelled());
            }
            other => panic!("expected task failure, got {other:?}"),
        }
    }

    #[test]
    fn test_resource_already_taken_uses_protocol_error() {
        let error = RpcError::resource_already_taken("message receiver");

        match error {
            RpcError::Protocol(ProtocolError::ResourceAlreadyTaken { resource }) => {
                assert_eq!(resource, "message receiver");
            }
            other => panic!("expected resource-taken error, got {other:?}"),
        }
    }

    #[test]
    fn test_service_error_round_trip() {
        let error = ServiceError {
            name: "MethodNotFound".to_string(),
            value: Value::from("missing"),
        };

        let encoded = Value::from(error);
        let decoded = ServiceError::try_from(encoded).unwrap();

        assert_eq!(decoded.name, "MethodNotFound");
        assert_eq!(decoded.value, Value::from("missing"));
    }

    #[test]
    fn test_service_error_try_from_requires_name_and_value() {
        let missing_name = Value::Map(vec![(Value::from("value"), Value::from("missing"))]);
        assert!(ServiceError::try_from(missing_name).is_err());

        let missing_value = Value::Map(vec![(Value::from("name"), Value::from("SomeError"))]);
        assert!(ServiceError::try_from(missing_value).is_err());
    }

    #[test]
    fn test_from_remote_error_value_preserves_service_errors() {
        let value = Value::Map(vec![
            (Value::from("name"), Value::from("SomeError")),
            (Value::from("value"), Value::from("payload")),
        ]);

        let error = RpcError::from_remote_error_value(value);

        match error {
            RpcError::Service(service_error) => {
                assert_eq!(service_error.name, "SomeError");
                assert_eq!(service_error.value, Value::from("payload"));
            }
            other => panic!("expected service error, got {other:?}"),
        }
    }

    #[test]
    fn test_from_remote_error_value_uses_fallback_names() {
        let malformed_map = Value::Map(vec![(Value::from("value"), Value::from("payload"))]);
        let error = RpcError::from_remote_error_value(malformed_map);

        match error {
            RpcError::Service(service_error) => {
                assert_eq!(service_error.name, "UnknownError");
                assert_eq!(
                    service_error.value,
                    Value::Map(vec![(Value::from("value"), Value::from("payload"),)])
                );
            }
            other => panic!("expected service error, got {other:?}"),
        }

        let scalar_error = RpcError::from_remote_error_value(Value::from("boom"));
        match scalar_error {
            RpcError::Service(service_error) => {
                assert_eq!(service_error.name, "RemoteError");
                assert_eq!(service_error.value, Value::from("boom"));
            }
            other => panic!("expected service error, got {other:?}"),
        }
    }
}
