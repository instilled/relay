//! JSON-RPC 2.0 envelopes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Num(u64),
    Str(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Id,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: Id, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: Id, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Server-initiated message: events, output streams. No id, no reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// Server-defined: the named run/flow/plugin does not exist.
    pub const NOT_FOUND: i64 = -32004;
    /// Someone else holds what was asked for (a run's keyboard).
    pub const BUSY: i64 = -32005;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    pub fn method_not_found(method: &str) -> Self {
        Self::new(Self::METHOD_NOT_FOUND, format!("method not found: {method}"))
    }
    pub fn invalid_params(msg: impl std::fmt::Display) -> Self {
        Self::new(Self::INVALID_PARAMS, format!("invalid params: {msg}"))
    }
    pub fn internal(msg: impl std::fmt::Display) -> Self {
        Self::new(Self::INTERNAL, msg.to_string())
    }
    pub fn not_found(what: &str, id: &str) -> Self {
        Self::new(Self::NOT_FOUND, format!("no such {what}: {id}"))
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_roundtrips_all_three_kinds() {
        let req = Request::new(Id::Num(1), "daemon.ping", None);
        let res = Response::ok(Id::Num(1), json!({"ok": true}));
        let note = Notification {
            jsonrpc: "2.0".into(),
            method: "event".into(),
            params: Some(json!({"k": 1})),
        };

        for (msg, want) in [
            (serde_json::to_string(&req).unwrap(), "request"),
            (serde_json::to_string(&res).unwrap(), "response"),
            (serde_json::to_string(&note).unwrap(), "notification"),
        ] {
            let got: Message = serde_json::from_str(&msg).unwrap();
            let kind = match got {
                Message::Request(_) => "request",
                Message::Response(_) => "response",
                Message::Notification(_) => "notification",
            };
            assert_eq!(kind, want, "wire: {msg}");
        }
    }

    #[test]
    fn error_response_carries_code() {
        let res = Response::err(Id::Str("a".into()), RpcError::method_not_found("x"));
        let s = serde_json::to_string(&res).unwrap();
        let back: Response = serde_json::from_str(&s).unwrap();
        assert_eq!(back.error.unwrap().code, RpcError::METHOD_NOT_FOUND);
        assert!(back.result.is_none());
    }
}
