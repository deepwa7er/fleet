//! The client↔server protocol (docs/remote.md §4): JSON-RPC 2.0 with LSP's
//! `Content-Length` framing — the same codec the LSP client uses, shared
//! here. Method names and parameter shapes are the wire contract; bump
//! [`PROTOCOL_VERSION`] on any breaking change and the initialize handshake
//! turns skew into an instructive error instead of undefined behavior.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bump on breaking protocol changes.
pub const PROTOCOL_VERSION: u32 = 1;

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const SHUTDOWN: &str = "shutdown";

    pub const READ_DIR: &str = "workspace/readDir";
    pub const READ_FILE: &str = "workspace/readFile";
    pub const LIST_FILES: &str = "workspace/listFiles";
    pub const SEARCH_TEXT: &str = "workspace/searchText";

    pub const DOC_OPEN: &str = "document/didOpen";
    pub const DOC_CHANGE: &str = "document/didChange";
    pub const DOC_SAVE: &str = "document/save";
    pub const DOC_CLOSE: &str = "document/didClose";
    /// Server→client notification: the auto-save sync readout.
    pub const SYNC_STATE: &str = "document/syncState";
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InitializeParams {
    pub version: u32,
    /// Workspace path as the user typed it; relative paths resolve against
    /// the server's $HOME (scp-style `ide desktop:code/fleet`).
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InitializeResult {
    pub version: u32,
    /// The canonicalized root the server actually opened.
    pub root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathParams {
    pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchTextParams {
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocTextParams {
    pub path: PathBuf,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncStateParams {
    pub synced: bool,
}

pub mod codec {
    use anyhow::{Context as _, Result};
    use futures::{AsyncBufReadExt as _, AsyncReadExt as _};
    use serde::Serialize;
    use serde_json::Value;

    /// One `Content-Length`-framed JSON-RPC message, ready to write.
    pub fn frame(message: &impl Serialize) -> Vec<u8> {
        let body = serde_json::to_vec(message).expect("rpc messages serialize");
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend(body);
        frame
    }

    /// Read one framed message; errors on EOF or malformed framing.
    pub async fn read_message<R>(reader: &mut R) -> Result<Value>
    where
        R: futures::AsyncBufRead + Unpin,
    {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                anyhow::bail!("eof");
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = Some(value.trim().parse()?);
            }
        }
        let content_length = content_length.context("missing Content-Length header")?;
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }
}
