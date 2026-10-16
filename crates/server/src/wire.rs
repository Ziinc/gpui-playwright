//! Playwright's message shapes: calls, results, errors and serialized values.

use serde_json::{Map, Value, json};
use std::time::{Duration, Instant};

/// A method call from the client: `{id, guid, method, params, metadata}`.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub id: Value,
    pub guid: String,
    pub method: String,
    pub params: Value,
    /// Milliseconds from the call's metadata; 0 means no timeout.
    pub timeout: u64,
}

impl Call {
    pub fn parse(text: &str) -> Result<Self, String> {
        let message: Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let guid = message
            .get("guid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or("message without method")?
            .to_owned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let timeout = message
            .pointer("/metadata/timeout")
            .and_then(Value::as_f64)
            .unwrap_or(0.) as u64;
        Ok(Self {
            id,
            guid,
            method,
            params,
            timeout,
        })
    }

    pub fn deadline(&self) -> Option<Instant> {
        (self.timeout > 0).then(|| Instant::now() + Duration::from_millis(self.timeout))
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(Value::as_str)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.params
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn f64(&self, key: &str) -> Option<f64> {
        self.params.get(key).and_then(Value::as_f64)
    }
}

/// An error in the shape Playwright clients rethrow.
#[derive(Debug, Clone)]
pub(crate) struct PwError {
    pub name: &'static str,
    pub message: String,
    pub log: Vec<String>,
    pub details: Option<Value>,
}

impl PwError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            name: "Error",
            message: message.into(),
            log: Vec::new(),
            details: None,
        }
    }

    pub fn unsupported(what: &str) -> Self {
        Self::new(format!(
            "{what} is not supported by GPUI apps (gpui-playwright implements the parts of Playwright that map onto a desktop app)"
        ))
    }

    pub fn timeout(timeout: u64, log: Vec<String>) -> Self {
        Self {
            name: "TimeoutError",
            message: format!("Timeout {timeout}ms exceeded."),
            log,
            details: None,
        }
    }

    pub fn closed() -> Self {
        Self {
            name: "TargetClosedError",
            message: "Target page, context or browser has been closed".into(),
            log: Vec::new(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_log(mut self, log: Vec<String>) -> Self {
        self.log = log;
        self
    }

    pub fn into_response(self, id: &Value) -> Value {
        let stack = format!("{}: {}", self.name, self.message);
        let mut response = json!({
            "id": id,
            "error": {"error": {"message": self.message, "name": self.name, "stack": stack}},
        });
        if !self.log.is_empty() {
            response["log"] = json!(self.log);
        }
        if let Some(details) = self.details {
            response["errorDetails"] = details;
        }
        response
    }
}

/// Always carries a `result` object: void methods accept `{}`, and methods
/// whose result fields are all optional reject a missing result.
pub(crate) fn result_response(id: &Value, result: Value) -> Value {
    match result {
        Value::Null => json!({"id": id, "result": {}}),
        result => json!({"id": id, "result": result}),
    }
}

pub(crate) fn event(guid: &str, method: &str, params: Value) -> Value {
    json!({"guid": guid, "method": method, "params": params})
}

/// Playwright's `SerializedValue` encoding.
pub(crate) mod serialized {
    use super::*;

    pub fn string(value: &str) -> Value {
        json!({"s": value})
    }

    pub fn number(value: f64) -> Value {
        json!({"n": value})
    }

    pub fn boolean(value: bool) -> Value {
        json!({"b": value})
    }

    pub fn null() -> Value {
        json!({"v": "null"})
    }

    pub fn array(values: Vec<Value>) -> Value {
        json!({"a": values, "id": 1})
    }

    /// Decodes a `SerializedValue` into plain JSON, enough for option objects.
    pub fn decode(value: &Value) -> Value {
        let Some(object) = value.as_object() else {
            return Value::Null;
        };
        if let Some(s) = object.get("s") {
            return s.clone();
        }
        if let Some(n) = object.get("n") {
            return n.clone();
        }
        if let Some(b) = object.get("b") {
            return b.clone();
        }
        if let Some(a) = object.get("a").and_then(Value::as_array) {
            return Value::Array(a.iter().map(decode).collect());
        }
        if let Some(o) = object.get("o").and_then(Value::as_array) {
            let mut map = Map::new();
            for entry in o {
                if let Some(key) = entry.get("k").and_then(Value::as_str) {
                    map.insert(
                        key.to_owned(),
                        entry.get("v").map(decode).unwrap_or_default(),
                    );
                }
            }
            return Value::Object(map);
        }
        Value::Null
    }
}
