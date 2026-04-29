//! UnixStream transport: framing + request/response correlation.
//!
//! Thin wrapper around [`tokio::net::UnixStream`] that delegates the
//! length-delimited frame I/O to [`crate::daemon::protocol`]. One
//! [`Transport`] owns one connection; the higher-level [`super::Client`]
//! is the "open call" surface.

use std::path::Path;
use tokio::net::UnixStream;

use crate::daemon::protocol::{Request, Response, read_frame_bytes, write_request};

use super::errors::{ClientError, ClientResult};

pub struct Transport {
    stream: UnixStream,
}

impl Transport {
    /// Connect to the daemon socket at `path`. Maps `ECONNREFUSED` /
    /// `ENOENT` to [`ClientError::ConnectionRefused`] so the auto-start
    /// path can recognize "no daemon listening".
    pub async fn connect(path: &Path) -> ClientResult<Self> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(ClientError::ConnectionRefused)?;
        Ok(Self { stream })
    }

    /// Send `req`, await one response frame, decode and return it.
    /// Slice 0 makes one call per [`Transport`] — multiplexing /
    /// pipelining lands in a later slice (StreamFrame consumer).
    pub async fn call(&mut self, req: Request) -> ClientResult<Response> {
        write_request(&mut self.stream, &req).await?;
        let body = read_frame_bytes(&mut self.stream).await?;
        let resp: Response = serde_json::from_slice(&body)?;
        Ok(resp)
    }
}
