use serde_json::{json, Value};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// --- Error types ---

#[derive(Debug)]
pub enum ElectrumError {
    Connection(String),
    Protocol(String),
    Rpc { code: i64, message: String },
}

impl fmt::Display for ElectrumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(msg) => write!(f, "connection error: {msg}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Self::Rpc { code, message } => write!(f, "RPC error {code}: {message}"),
        }
    }
}

impl std::error::Error for ElectrumError {}

impl From<std::io::Error> for ElectrumError {
    fn from(e: std::io::Error) -> Self {
        Self::Connection(e.to_string())
    }
}

impl From<serde_json::Error> for ElectrumError {
    fn from(e: serde_json::Error) -> Self {
        Self::Protocol(e.to_string())
    }
}

// --- Connection wrapper using trait objects ---

struct Connection {
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    writer: Box<dyn AsyncWrite + Unpin + Send>,
}

// --- Client ---

pub struct ElectrumClient {
    host: String,
    port: u16,
    tls: bool,
    next_id: AtomicU64,
    conn: Mutex<Option<Connection>>,
}

impl ElectrumClient {
    pub fn new(host: String, port: u16) -> Self {
        Self {
            host,
            port,
            tls: false,
            next_id: AtomicU64::new(1),
            conn: Mutex::new(None),
        }
    }

    pub fn with_tls(host: String, port: u16, tls: bool) -> Self {
        Self {
            host,
            port,
            tls,
            next_id: AtomicU64::new(1),
            conn: Mutex::new(None),
        }
    }

    /// Connect to Fulcrum and perform server.version handshake.
    pub async fn connect(&self) -> Result<(), ElectrumError> {
        let addr = format!("{}:{}", self.host, self.port);
        info!(
            "connecting to Fulcrum at {addr}{}",
            if self.tls { " (TLS)" } else { "" }
        );

        let tcp_stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| ElectrumError::Connection(format!("failed to connect to {addr}: {e}")))?;

        let connection = if self.tls {
            self.wrap_tls(tcp_stream).await?
        } else {
            let (read_half, write_half) = tcp_stream.into_split();
            Connection {
                reader: BufReader::new(Box::new(read_half)),
                writer: Box::new(write_half),
            }
        };

        {
            let mut guard = self.conn.lock().await;
            *guard = Some(connection);
        }

        // Handshake
        let version = self
            .raw_request("server.version", &json!(["fulcrum-rust", "1.5"]))
            .await?;
        info!("Fulcrum handshake OK: {version}");

        Ok(())
    }

    async fn wrap_tls(&self, tcp_stream: TcpStream) -> Result<Connection, ElectrumError> {
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        // Fulcrum typically uses self-signed certs, so add a custom verifier
        // that accepts any certificate (like Node.js rejectUnauthorized: false)
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
            .with_no_client_auth();

        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

        let dns_name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|e| ElectrumError::Connection(format!("invalid server name: {e}")))?;

        let tls_stream = connector
            .connect(dns_name, tcp_stream)
            .await
            .map_err(|e| ElectrumError::Connection(format!("TLS handshake failed: {e}")))?;

        let (read_half, write_half) = tokio::io::split(tls_stream);
        Ok(Connection {
            reader: BufReader::new(Box::new(read_half)),
            writer: Box::new(write_half),
        })
    }

    /// Send a JSON-RPC request and return the result value.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ElectrumError> {
        match self.raw_request(method, &params).await {
            Ok(val) => Ok(val),
            Err(ElectrumError::Connection(msg)) => {
                warn!("request failed ({msg}), attempting reconnect");
                self.reconnect().await?;
                self.raw_request(method, &params).await
            }
            Err(e) => Err(e),
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.conn.lock().await.is_some()
    }

    pub async fn reconnect(&self) -> Result<(), ElectrumError> {
        info!("reconnecting to Fulcrum");
        {
            let mut guard = self.conn.lock().await;
            *guard = None;
        }
        self.connect().await
    }

    /// Low-level: send request and read one response line. Caller holds no lock.
    async fn raw_request(&self, method: &str, params: &Value) -> Result<Value, ElectrumError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let req = json!({
            "method": method,
            "params": params,
            "id": id,
        });

        let mut line = String::new();

        {
            let mut guard = self.conn.lock().await;
            let conn = guard
                .as_mut()
                .ok_or_else(|| ElectrumError::Connection("not connected".into()))?;

            let mut msg = serde_json::to_string(&req)?;
            msg.push('\n');
            debug!("-> {msg}");

            conn.writer.write_all(msg.as_bytes()).await?;

            line.clear();
            let n = conn.reader.read_line(&mut line).await?;
            if n == 0 {
                *guard = None;
                return Err(ElectrumError::Connection(
                    "connection closed by peer".into(),
                ));
            }
        }

        debug!("<- {line}");
        let resp: Value = serde_json::from_str(line.trim())?;

        // Verify response ID matches
        if resp.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(ElectrumError::Protocol(format!(
                "response id mismatch: expected {id}, got {}",
                resp.get("id").unwrap_or(&Value::Null)
            )));
        }

        // Check for RPC error
        if let Some(err_obj) = resp.get("error") {
            let code = err_obj.get("code").and_then(Value::as_i64).unwrap_or(-1);
            let message = err_obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            return Err(ElectrumError::Rpc { code, message });
        }

        resp.get("result")
            .cloned()
            .ok_or_else(|| ElectrumError::Protocol("response missing 'result' field".into()))
    }
}

// --- TLS: Accept any certificate (Fulcrum often uses self-signed) ---

#[derive(Debug)]
struct NoCertificateVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

// --- Helpers for building JSON-RPC params ---

#[cfg(test)]
pub fn build_request(method: &str, params: Value, id: u64) -> Value {
    json!({
        "method": method,
        "params": params,
        "id": id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_format() {
        let req = build_request("server.version", json!(["client", "1.5"]), 1);
        assert_eq!(req["method"], "server.version");
        assert_eq!(req["params"][0], "client");
        assert_eq!(req["params"][1], "1.5");
        assert_eq!(req["id"], 1);
    }

    #[test]
    fn build_request_serializes_to_valid_json() {
        let req = build_request("blockchain.address.get_balance", json!(["addr123"]), 42);
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"method\":\"blockchain.address.get_balance\""));
        assert!(s.contains("\"id\":42"));
    }

    #[test]
    fn error_display() {
        let e = ElectrumError::Connection("timeout".into());
        assert_eq!(e.to_string(), "connection error: timeout");

        let e = ElectrumError::Protocol("bad json".into());
        assert_eq!(e.to_string(), "protocol error: bad json");

        let e = ElectrumError::Rpc {
            code: -32600,
            message: "invalid request".into(),
        };
        assert_eq!(e.to_string(), "RPC error -32600: invalid request");
    }

    #[tokio::test]
    async fn client_not_connected_returns_error() {
        let client = ElectrumClient::new("127.0.0.1".into(), 59999);
        assert!(!client.is_connected().await);

        let result = client.raw_request("server.ping", &json!([])).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ElectrumError::Connection(msg) => assert!(msg.contains("not connected")),
            other => panic!("expected Connection error, got: {other}"),
        }
    }

    #[test]
    fn parse_success_response() {
        let resp: Value =
            serde_json::from_str(r#"{"result":["Fulcrum 2.1.0","1.5"],"id":1}"#).unwrap();
        let result = resp.get("result").unwrap();
        assert!(result.is_array());
        assert_eq!(result[0], "Fulcrum 2.1.0");
    }

    #[test]
    fn parse_error_response() {
        let resp: Value =
            serde_json::from_str(r#"{"error":{"code":-32600,"message":"invalid request"},"id":1}"#)
                .unwrap();
        let err = resp.get("error").unwrap();
        assert_eq!(err["code"], -32600);
        assert_eq!(err["message"], "invalid request");
    }

    #[tokio::test]
    async fn with_tls_constructor() {
        let client = ElectrumClient::with_tls("127.0.0.1".into(), 50002, true);
        assert!(!client.is_connected().await);
        assert!(client.tls);
    }
}
