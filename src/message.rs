//! Defines the MessagePack-RPC message types and their serialization/deserialization.
//!
//! Includes structures for requests, responses, and notifications, as well as
//! utilities for encoding and decoding these messages.
use std::{
    convert::TryFrom,
    io::{Read, Write},
    result,
};

use rmpv::{
    Value,
    decode::{self, read_value},
    encode::write_value,
};

use crate::error::*;

/// Message type identifier for requests.
const REQUEST_MESSAGE: u64 = 0;
/// Message type identifier for responses.
const RESPONSE_MESSAGE: u64 = 1;
/// Message type identifier for notifications.
const NOTIFICATION_MESSAGE: u64 = 2;

/// Represents the different types of RPC messages: requests, responses, and notifications.
#[derive(PartialEq, Clone, Debug)]
pub enum Message {
    /// An RPC request.
    Request(Request),
    /// An RPC response.
    Response(Response),
    /// An RPC notification.
    Notification(Notification),
}

/// An RPC request message containing an ID, method name, and parameters.
#[derive(PartialEq, Clone, Debug)]
pub struct Request {
    /// Unique identifier for matching responses.
    pub id: u32,
    /// The method name to invoke.
    pub method: String,
    /// Arguments to pass to the method.
    pub params: Vec<Value>,
}

/// An RPC response message containing an ID and either a result or an error.
#[derive(PartialEq, Clone, Debug)]
pub struct Response {
    /// Matches the request ID this is responding to.
    pub id: u32,
    /// The result value or error value.
    pub result: result::Result<Value, Value>,
}

/// An RPC notification message containing a method name and parameters.
#[derive(PartialEq, Clone, Debug)]
pub struct Notification {
    /// The method name to invoke.
    pub method: String,
    /// Arguments to pass to the method.
    pub params: Vec<Value>,
}

impl Message {
    /// Converts the message to a MessagePack-RPC compatible Value.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Request(req) => Value::Array(vec![
                Value::Integer(REQUEST_MESSAGE.into()),
                Value::Integer(req.id.into()),
                Value::String(req.method.clone().into()),
                Value::Array(req.params.clone()),
            ]),
            Self::Response(resp) => {
                let (error, result) = match &resp.result {
                    Ok(value) => (Value::Nil, value.clone()),
                    Err(error) => (error.clone(), Value::Nil),
                };
                Value::Array(vec![
                    Value::Integer(RESPONSE_MESSAGE.into()),
                    Value::Integer(resp.id.into()),
                    error,
                    result,
                ])
            }
            Self::Notification(notif) => Value::Array(vec![
                Value::Integer(NOTIFICATION_MESSAGE.into()),
                Value::String(notif.method.clone().into()),
                Value::Array(notif.params.clone()),
            ]),
        }
    }

    /// Creates a Message from a MessagePack-RPC compatible Value.
    pub fn from_value(value: Value) -> Result<Self> {
        let array = match value {
            Value::Array(array) => array,
            _ => return Err(RpcError::Protocol("Invalid message format".into())),
        };

        let Some((message_type, fields)) = array.split_first() else {
            return Err(RpcError::Protocol("Empty message array".into()));
        };

        match parse_message_type(message_type)? {
            REQUEST_MESSAGE => parse_request(fields),
            RESPONSE_MESSAGE => parse_response(fields),
            NOTIFICATION_MESSAGE => parse_notification(fields),
            other => Err(RpcError::Protocol(ProtocolError::InvalidMessageType(other))),
        }
    }

    /// Encodes the message to MessagePack format and writes it to the given writer.
    pub fn encode<W: Write>(&self, writer: &mut W) -> Result<()> {
        let value = self.to_value();
        write_value(writer, &value)?;
        Ok(())
    }

    /// Reads and decodes a message from MessagePack format using the given reader.
    pub fn decode<R: Read>(reader: &mut R) -> Result<Self> {
        match read_value(reader) {
            Ok(value) => Self::from_value(value),
            Err(decode::Error::InvalidMarkerRead(e) | decode::Error::InvalidDataRead(e)) => {
                Err(RpcError::from(e))
            }
            Err(decode::Error::DepthLimitExceeded) => {
                Err(RpcError::Protocol(ProtocolError::DepthLimitExceeded))
            }
        }
    }
}

/// Parses the leading message type tag from a MessagePack-RPC array.
fn parse_message_type(value: &Value) -> Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| RpcError::Protocol("Invalid message type".into()))
}

/// Parses a MessagePack-RPC request body.
fn parse_request(fields: &[Value]) -> Result<Message> {
    let [id, method, params] = fields else {
        return Err(RpcError::Protocol("Invalid request message length".into()));
    };

    Ok(Message::Request(Request {
        id: parse_message_id(id, "request")?,
        method: parse_method_name(method, "request")?,
        params: parse_params(params, "request")?,
    }))
}

/// Parses a MessagePack-RPC response body.
fn parse_response(fields: &[Value]) -> Result<Message> {
    let [id, error, result] = fields else {
        return Err(RpcError::Protocol("Invalid response message length".into()));
    };

    let result = if matches!(error, Value::Nil) {
        Ok(result.clone())
    } else {
        Err(error.clone())
    };

    Ok(Message::Response(Response {
        id: parse_message_id(id, "response")?,
        result,
    }))
}

/// Parses a MessagePack-RPC notification body.
fn parse_notification(fields: &[Value]) -> Result<Message> {
    let [method, params] = fields else {
        return Err(RpcError::Protocol(
            "Invalid notification message length".into(),
        ));
    };

    Ok(Message::Notification(Notification {
        method: parse_method_name(method, "notification")?,
        params: parse_params(params, "notification")?,
    }))
}

/// Parses a method name field from a request or notification.
fn parse_method_name(value: &Value, context: &str) -> Result<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| RpcError::Protocol(format!("Invalid {context} method").into()))
}

/// Parses a parameter list field from a request or notification.
fn parse_params(value: &Value, context: &str) -> Result<Vec<Value>> {
    match value {
        Value::Array(params) => Ok(params.clone()),
        _ => Err(RpcError::Protocol(
            format!("Invalid {context} params").into(),
        )),
    }
}

/// Parses a wire message id and rejects values that exceed the public `u32` range.
fn parse_message_id(value: &Value, context: &str) -> Result<u32> {
    let raw_id = value
        .as_u64()
        .ok_or_else(|| RpcError::Protocol(format!("Invalid {context} id").into()))?;

    u32::try_from(raw_id).map_err(|_| RpcError::Protocol(format!("Invalid {context} id").into()))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    // Test cases at the top level of the test module
    lazy_static::lazy_static! {
        static ref TEST_CASES: Vec<Message> = vec![
            Message::Request(Request {
                id: 1,
                method: "test_method".to_string(),
                params: vec![Value::String("param1".into()), Value::Integer(42.into())],
            }),
            Message::Response(Response {
                id: 2,
                result: Ok(Value::String("success".into())),
            }),
            Message::Response(Response {
                id: 3,
                result: Err(Value::String("error".into())),
            }),
            Message::Notification(Notification {
                method: "test_notification".to_string(),
                params: vec![Value::Boolean(true), Value::F64(2.14)],
            }),
            Message::Request(Request {
                id: 4,
                method: "complex_method".to_string(),
                params: vec![
                    Value::Array(vec![Value::String("nested".into()), Value::Integer(1.into())]),
                    Value::Map(vec![
                        (Value::String("key".into()), Value::Boolean(true)),
                        (Value::String("value".into()), Value::F64(1.718)),
                    ]),
                ],
            }),
        ];
    }

    #[test]
    fn test_message_idempotence_and_invalid_inputs() {
        // Helper function to test idempotence
        fn assert_idempotence(message: &Message) {
            let value = message.to_value();
            let roundtrip_message = Message::from_value(value).unwrap();
            assert_eq!(message, &roundtrip_message);
        }

        // Test idempotence for all cases
        for message in TEST_CASES.iter() {
            assert_idempotence(message);
        }

        // Test invalid inputs
        let invalid_values = vec![
            Value::Nil,
            Value::Boolean(true),
            Value::Integer(42.into()),
            Value::String("not an array".into()),
            Value::Array(vec![]),
            Value::Array(vec![Value::Integer(999.into())]), // Invalid message type
            Value::Array(vec![Value::Integer(REQUEST_MESSAGE.into())]), // Incomplete request
        ];

        for invalid_value in invalid_values {
            assert!(Message::from_value(invalid_value).is_err());
        }
    }

    #[test]
    fn test_message_round_trip_with_buffer() {
        for original_message in TEST_CASES.iter() {
            // Serialize the message to a buffer
            let mut write_buffer = Vec::new();
            original_message.encode(&mut write_buffer).unwrap();

            // Deserialize the message from the buffer
            let mut read_buffer = Cursor::new(write_buffer);
            let deserialized_message = Message::decode(&mut read_buffer).unwrap();

            // Assert that the deserialized message matches the original
            assert_eq!(original_message, &deserialized_message);

            // Ensure the entire buffer was consumed
            assert_eq!(read_buffer.position() as usize, read_buffer.get_ref().len());
        }
    }

    #[test]
    fn test_rejects_message_ids_outside_u32_range() {
        let request = Value::Array(vec![
            Value::Integer(REQUEST_MESSAGE.into()),
            Value::Integer((u64::from(u32::MAX) + 1).into()),
            Value::String("overflow".into()),
            Value::Array(vec![]),
        ]);
        assert!(Message::from_value(request).is_err());

        let response = Value::Array(vec![
            Value::Integer(RESPONSE_MESSAGE.into()),
            Value::Integer((u64::from(u32::MAX) + 1).into()),
            Value::Nil,
            Value::Nil,
        ]);
        assert!(Message::from_value(response).is_err());
    }
}
