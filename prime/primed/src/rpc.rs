//! Minimal Bitcoin JSON-RPC over HTTP/1.1 on tokio. One request per connection; the node
//! is local and we call it a couple of times a second at most.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Debug)]
pub struct Rpc {
    host: String,
    port: u16,
    path: String,
    auth: Option<String>,
    cookie: Option<std::path::PathBuf>,
    timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http {0}")]
    Http(u16),
    #[error("bad response: {0}")]
    Malformed(String),
    #[error("rpc error {code}: {message}")]
    Node { code: i64, message: String },
    #[error("timed out")]
    Timeout,
}

impl Rpc {
    pub fn new(url: &str, cookie: Option<&Path>, user: Option<&str>, pass: Option<&str>) -> Result<Rpc, String> {
        let rest = url.strip_prefix("http://").ok_or("rpc url must start with http://")?;
        let (hostport, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h.trim_matches(['[', ']']).to_string(), p.parse::<u16>().map_err(|_| "bad rpc port")?),
            None => (hostport.to_string(), 8332),
        };
        let auth = match (user, pass) {
            (Some(u), Some(p)) => Some(base64(format!("{u}:{p}").as_bytes())),
            _ => None,
        };
        Ok(Rpc {
            host,
            port,
            path: path.to_string(),
            auth,
            cookie: cookie.map(Path::to_path_buf),
            timeout: Duration::from_secs(20),
        })
    }

    fn auth_header(&self) -> Result<String, RpcError> {
        if let Some(c) = &self.cookie {
            let s = std::fs::read_to_string(c)?;
            return Ok(base64(s.trim().as_bytes()));
        }
        self.auth.clone().ok_or_else(|| RpcError::Malformed("no rpc credentials".into()))
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({"jsonrpc": "1.0", "id": "primed", "method": method, "params": params}).to_string();
        let req = format!(
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.path,
            self.host,
            self.port,
            self.auth_header()?,
            body.len(),
            body
        );
        let fut = async {
            let mut s = TcpStream::connect((self.host.as_str(), self.port)).await?;
            s.write_all(req.as_bytes()).await?;
            let mut buf = Vec::with_capacity(4096);
            s.read_to_end(&mut buf).await?;
            parse_response(&buf)
        };
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err(RpcError::Timeout),
        }
    }

    pub async fn getblockchaininfo(&self) -> Result<Value, RpcError> {
        self.call("getblockchaininfo", json!([])).await
    }

    pub async fn submitblock(&self, hex: &str) -> Result<Value, RpcError> {
        self.call("submitblock", json!([hex])).await
    }

    pub async fn getblockheader(&self, hash: &str) -> Result<Value, RpcError> {
        self.call("getblockheader", json!([hash, true])).await
    }
}

fn parse_response(buf: &[u8]) -> Result<Value, RpcError> {
    let sep = find(buf, b"\r\n\r\n").ok_or_else(|| RpcError::Malformed("no header terminator".into()))?;
    let head = std::str::from_utf8(&buf[..sep]).map_err(|_| RpcError::Malformed("non-utf8 header".into()))?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| RpcError::Malformed("no status".into()))?;
    let mut body = &buf[sep + 4..];
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let unchunked;
    if chunked {
        unchunked = dechunk(body)?;
        body = &unchunked;
    }
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) if status != 200 => return Err(RpcError::Http(status)),
        Err(e) => return Err(RpcError::Malformed(e.to_string())),
    };
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(RpcError::Node {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: err.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
        });
    }
    if status != 200 {
        return Err(RpcError::Http(status));
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, RpcError> {
    let mut out = Vec::with_capacity(b.len());
    loop {
        let nl = find(b, b"\r\n").ok_or_else(|| RpcError::Malformed("chunk size".into()))?;
        let size_str = std::str::from_utf8(&b[..nl]).map_err(|_| RpcError::Malformed("chunk size".into()))?;
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| RpcError::Malformed("chunk size".into()))?;
        b = &b[nl + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err(RpcError::Malformed("short chunk".into()));
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn parses_plain_and_chunked() {
        let r = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"result\":{\"blocks\":5},\"error\":null,\"id\":\"x\"}";
        assert_eq!(parse_response(r).unwrap()["blocks"], 5);
        let r = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"res\r\n14\r\nult\":7,\"error\":null}\r\n0\r\n\r\n";
        assert_eq!(parse_response(r).unwrap(), 7);
        let r = b"HTTP/1.1 500 Internal Server Error\r\n\r\n{\"result\":null,\"error\":{\"code\":-8,\"message\":\"bad\"},\"id\":\"x\"}";
        match parse_response(r) {
            Err(RpcError::Node { code: -8, message }) => assert_eq!(message, "bad"),
            other => panic!("{other:?}"),
        }
        let r = b"HTTP/1.1 401 Unauthorized\r\n\r\n";
        assert!(matches!(parse_response(r), Err(RpcError::Http(401))));
        let url = Rpc::new("http://127.0.0.1:9332", None, Some("u"), Some("p")).unwrap();
        assert_eq!((url.host.as_str(), url.port, url.path.as_str()), ("127.0.0.1", 9332, "/"));
    }
}
