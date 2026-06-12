use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcMessage {
    pub id: Option<u64>,
    pub method: Option<String>,
    pub params: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcMessage {
    #[allow(dead_code)]
    pub fn request(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            id: Some(id),
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            id: None,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    pub fn ok_response(id: u64, result: Value) -> Self {
        Self {
            id: Some(id),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }
}

// ── IPC transport — generic over the write half ───────────────────────────────

pub struct IpcTransport {
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    #[allow(dead_code)]
    next_id: u64,
}

impl IpcTransport {
    /// Use when the Rust core is a subprocess: write to its own stdout.
    pub fn from_stdout() -> Self {
        Self {
            writer: Box::new(tokio::io::stdout()),
            next_id: 1,
        }
    }

    pub async fn send(&mut self, msg: &RpcMessage) -> Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        debug!(msg = %line.trim(), "→ Swift");
        Ok(())
    }

    pub async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&RpcMessage::notification(method, params)).await
    }

    pub async fn respond(&mut self, id: u64, result: Value) -> Result<()> {
        self.send(&RpcMessage::ok_response(id, result)).await
    }
}

/// Spawn a reader task that forwards incoming lines from `reader` to a channel.
/// Use `tokio::io::stdin()` when the Rust core is a subprocess.
pub fn spawn_reader<R>(reader: R) -> mpsc::Receiver<RpcMessage>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            debug!(msg = %line, "← Swift");
            match serde_json::from_str::<RpcMessage>(&line) {
                Ok(msg) => {
                    if tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(e) => warn!("malformed RPC line: {e}: {line}"),
            }
        }
        debug!("IPC reader closed");
    });
    rx
}
