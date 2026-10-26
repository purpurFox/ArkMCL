//! Unix domain socket 上的 JSON-RPC 2.0 传输。
//!
//! 协议按行分隔：每条消息是一个 JSON 对象，以 `\n` 结尾。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::path::Path;

/// 解析错误。
pub const PARSE_ERROR: i32 = -32700;
/// 方法不存在。
pub const METHOD_NOT_FOUND: i32 = -32601;
/// 参数非法。
pub const INVALID_PARAMS: i32 = -32602;
/// 内部错误。
pub const INTERNAL_ERROR: i32 = -32603;

/// 单条消息上限。
const MAX_MESSAGE: usize = 1024 * 1024;

/// 当前 JSON-RPC 协议主版本。
///
/// 破坏性变更（删除/重命名方法、改变参数或结果语义）必须递增；
/// 新增字段或新增方法不递增。客户端应在首次连接时通过 "server.info" 校验。
pub const PROTOCOL_VERSION: u32 = 1;

/// JSON-RPC 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// 协议版本，固定为 `2.0`。
    pub jsonrpc: String,
    /// 请求 id；通知可为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 方法名。
    pub method: String,
    /// 参数。
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// 构造请求。
    #[must_use]
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id: Some(Value::from(id)),
            method: method.to_owned(),
            params,
        }
    }
}

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fault {
    /// 错误码。
    pub code: i32,
    /// 错误消息。
    pub message: String,
    /// 附加数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// 协议版本。
    pub jsonrpc: String,
    /// 对应请求 id。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// 成功结果。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 错误。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Fault>,
}

impl Response {
    /// 构造成功响应。
    #[must_use]
    pub fn result(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// 构造错误响应。
    #[must_use]
    pub fn fault(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(Fault {
                code,
                message,
                data: None,
            }),
        }
    }
}

/// 传输错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io(std::io::Error),
    /// JSON 编解码错误。
    Json(serde_json::Error),
    /// 对端未返回响应。
    Timeout,
    /// 消息超过上限。
    TooLarge,
    /// 当前平台不支持。
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Timeout => formatter.write_str("no response from service"),
            Self::TooLarge => formatter.write_str("message exceeds size limit"),
            Self::Unsupported => formatter.write_str("IPC is not supported on this platform"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Timeout | Self::TooLarge | Self::Unsupported => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// 发送一次请求并读取响应。
///
/// # Errors
///
/// 连接失败、超时、消息过大或 JSON 无法解析时返回错误。
#[cfg(unix)]
pub fn call(path: &Path, request: &Request) -> Result<Response, Error> {
    call_with(path, request, &mut |_| {})
}

/// 发送一次请求，并在等待响应期间接收服务端进度通知。
///
/// 通知是 `method = "progress"` 的 JSON-RPC 消息，不携带 `id`。
///
/// # Errors
///
/// 连接失败、超时、消息过大或 JSON 无法解析时返回错误。
#[cfg(unix)]
pub fn call_with(
    path: &Path,
    request: &Request,
    on_event: &mut dyn FnMut(&Value),
) -> Result<Response, Error> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(path)?;
    // Installer/download RPCs can legitimately run for minutes.
    stream.set_read_timeout(Some(Duration::from_secs(600)))?;
    stream.set_write_timeout(Some(Duration::from_secs(600)))?;

    let mut payload = serde_json::to_vec(request)?;
    payload.push(b'\n');
    stream.write_all(&payload)?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(Error::Timeout);
        }
        if line.len() > MAX_MESSAGE {
            return Err(Error::TooLarge);
        }
        let value: Value = serde_json::from_str(&line)?;
        if value.get("method").is_some() {
            if let Some(params) = value.get("params") {
                on_event(params);
            }
            continue;
        }
        return serde_json::from_value(value).map_err(Error::from);
    }
}

/// 非 Unix 平台的占位实现。
///
/// # Errors
///
/// 总是返回 `Unsupported`。
#[cfg(not(unix))]
pub fn call_with(
    _path: &Path,
    _request: &Request,
    _on_event: &mut dyn FnMut(&Value),
) -> Result<Response, Error> {
    Err(Error::Unsupported)
}

/// 发送一次请求并读取响应。
///
/// # Errors
///
/// 非 Unix 平台返回 `Unsupported`。
#[cfg(not(unix))]
pub fn call(_path: &Path, _request: &Request) -> Result<Response, Error> {
    Err(Error::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::{Request, Response};
    use serde_json::json;

    #[test]
    fn serializes_request() {
        let request = Request::new(1, "server.info", json!({}));
        let text = serde_json::to_string(&request).expect("json");
        assert!(text.contains("\"jsonrpc\":\"2.0\""));
        assert!(text.contains("\"method\":\"server.info\""));
    }

    #[test]
    fn builds_success_and_fault_responses() {
        let success = Response::result(Some(json!(1)), json!({"pong": true}));
        assert!(success.error.is_none());
        assert_eq!(success.result.expect("result")["pong"], true);

        let fault = Response::fault(Some(json!(2)), -32601, "missing".to_owned());
        assert!(fault.result.is_none());
        assert_eq!(fault.error.expect("fault").code, -32601);
    }
}
