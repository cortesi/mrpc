//! Defines the MessagePack-RPC message types and their serialization/deserialization.
//!
//! Includes structures for requests, responses, and notifications, as well as
//! utilities for encoding and decoding these messages.
use rmpv::Value;
use std::io::{Read, Write};

use crate::error::*;

const REQUEST_MESSAGE: u64 = 0;
const RESPONSE_MESSAGE: u64 = 1;
const NOTIFICATION_MESSAGE: u64 = 2;

/// Represents the different types of RPC messages: requests, responses, and notifications.
#[derive(PartialEq, Clone, Debug)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

/// An RPC request message containing an ID, method name, and parameters.
#[derive(PartialEq, Clone, Debug)]
pub struct Request {
    pub id: u32,
    pub method: String,
    pub params: Vec<Value>,
}

/// An RPC response message containing an ID and either a result or an error.
#[derive(PartialEq, Clone, Debug)]
pub struct Response {
    pub id: u32,
    pub result: std::result::Result<Value, Value>,
}

/// An RPC notification message containing a method name and parameters.
#[derive(PartialEq, Clone, Debug)]
pub struct Notification {
    pub method: String,
    pub params: Vec<Value>,
}

impl Message {
    /// Converts the message to a MessagePack-RPC compatible Value.
    pub fn to_value(&self) -> Value {
        match self {
            Message::Request(req) => Value::Array(vec![
                Value::Integer(REQUEST_MESSAGE.into()),
                Value::Integer(req.id.into()),
                Value::String(req.method.clone().into()),
                Value::Array(req.params.clone()),
            ]),
            Message::Response(resp) => Value::Array(vec![
                Value::Integer(RESPONSE_MESSAGE.into()),
                Value::Integer(resp.id.into()),
                match &resp.result {
                    Ok(_value) => Value::Nil,
                    Err(err) => err.clone(),
                },
                match &resp.result {
                    Ok(value) => value.clone(),
                    Err(_) => Value::Nil,
                },
            ]),
            Message::Notification(notif) => Value::Array(vec![
                Value::Integer(NOTIFICATION_MESSAGE.into()),
                Value::String(notif.method.clone().into()),
                Value::Array(notif.params.clone()),
            ]),
        }
    }

    /// Creates a Message from a MessagePack-RPC compatible Value.
    pub fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Array(array) => {
                if array.is_empty() {
                    return Err(RpcError::Protocol("Empty message array".into()));
                }
                match array[0] {
                    Value::Integer(msg_type) => match msg_type.as_u64() {
                        Some(REQUEST_MESSAGE) => {
                            if array.len() != 4 {
                                return Err(RpcError::Protocol(
                                    "Invalid request message length".into(),
                                ));
                            }
                            let id = array[1]
                                .as_u64()
                                .ok_or(RpcError::Protocol("Invalid request id".into()))?
                                as u32;
                            let method = array[2]
                                .as_str()
                                .ok_or(RpcError::Protocol("Invalid request method".into()))?
                                .to_string();
                            let params = match &array[3] {
                                Value::Array(params) => params.clone(),
                                _ => {
                                    return Err(RpcError::Protocol("Invalid request params".into()))
                                }
                            };
                            Ok(Message::Request(Request { id, method, params }))
                        }
                        Some(RESPONSE_MESSAGE) => {
                            if array.len() != 4 {
                                return Err(RpcError::Protocol(
                                    "Invalid response message length".into(),
                                ));
                            }
                            let id = array[1]
                                .as_u64()
                                .ok_or(RpcError::Protocol("Invalid response id".into()))?
                                as u32;
                            let result = if array[2] == Value::Nil {
                                Ok(array[3].clone())
                            } else {
                                Err(array[2].clone())
                            };
                            Ok(Message::Response(Response { id, result }))
                        }
                        Some(NOTIFICATION_MESSAGE) => {
                            if array.len() != 3 {
                                return Err(RpcError::Protocol(
                                    "Invalid notification message length".into(),
                                ));
                            }
                            let method = array[1]
                                .as_str()
                                .ok_or(RpcError::Protocol("Invalid notification method".into()))?
                                .to_string();
                            let params = match &array[2] {
                                Value::Array(params) => params.clone(),
                                _ => {
                                    return Err(RpcError::Protocol(
                                        "Invalid notification params".into(),
                                    ))
                                }
                            };
                            Ok(Message::Notification(Notification { method, params }))
                        }
                        _ => Err(RpcError::Protocol("Invalid message type".into())),
                    },
                    _ => Err(RpcError::Protocol("Invalid message type".into())),
                }
            }
            _ => Err(RpcError::Protocol("Invalid message format".into())),
        }
    }

    /// Encodes the message to MessagePack format and writes it to the given writer.
    pub fn encode<W: Write>(&self, writer: &mut W) -> Result<()> {
        let value = self.to_value();
        rmpv::encode::write_value(writer, &value)?;
        Ok(())
    }

    /// Reads and decodes a message from MessagePack format using the given reader.
    pub fn decode<R: Read>(reader: &mut R) -> Result<Self> {
        match rmpv::decode::read_value(reader) {
            Ok(value) => Self::from_value(value),
            Err(rmpv::decode::Error::InvalidMarkerRead(e))
            | Err(rmpv::decode::Error::InvalidDataRead(e)) => Err(RpcError::from(e)),
            Err(rmpv::decode::Error::DepthLimitExceeded) => {
                Err(RpcError::Protocol("Depth limit exceeded".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
}
