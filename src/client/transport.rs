//! UnixStream transport: framing + request/response correlation.
//!
//! Thin wrapper around [`tokio::net::UnixStream`] that delegates the
//! length-delimited frame I/O to [`crate::daemon::protocol`]. One
//! [`Transport`] owns one connection; the higher-level [`super::Client`]
//! is the "open call" surface.

use std::path::Path;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use crate::daemon::protocol::{Request, Response, StreamFrame, read_frame_bytes, write_request};

use super::errors::{ClientError, ClientResult};

/// A handle returned by [`Transport::call_stream`]. The caller must
/// drain [`StreamFrame`]s via `recv().await` and finally await
/// `done` for the terminal [`Response`].
#[allow(dead_code)]
pub struct StreamFrames {
    pub frames: mpsc::Receiver<StreamFrame>,
    pub done: oneshot::Receiver<ClientResult<Response>>,
}

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
    /// Slice 0 makes one call per [Transport] -- multiplexing /
    /// pipelining lands in a later slice (StreamFrame consumer).
    pub async fn call(&mut self, req: Request) -> ClientResult<Response> {
        write_request(&mut self.stream, &req).await?;
        let body = read_frame_bytes(&mut self.stream).await?;
        let resp: Response = serde_json::from_slice(&body)?;
        Ok(resp)
    }

    /// Send a streaming request and return a handle that lets the
    /// caller drain `StreamFrame`s followed by awaiting the final
    /// `Response`.
    #[allow(dead_code)]
    pub async fn call_stream(&mut self, req: Request) -> ClientResult<StreamFrames> {
        write_request(&mut self.stream, &req).await?;

        let (frame_tx, frame_rx) = mpsc::channel(16);
        let (done_tx, done_rx) = oneshot::channel();

        // Steal the stream so the background task owns it.
        let mut stream = std::mem::replace(
            &mut self.stream,
            UnixStream::from_std(std::os::unix::net::UnixStream::pair().unwrap().0).unwrap(),
        );

        tokio::spawn(async move {
            loop {
                match read_frame_bytes(&mut stream).await {
                    Ok(body) => {
                        // First try Response (final frame).
                        if let Ok(resp) = serde_json::from_slice::<Response>(&body) {
                            let _ = done_tx.send(Ok(resp));
                            return;
                        }
                        // Otherwise assume StreamFrame.
                        match serde_json::from_slice::<StreamFrame>(&body) {
                            Ok(frame) => {
                                if frame_tx.send(frame).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = done_tx.send(Err(ClientError::Protocol(format!(
                                    "failed to decode frame: {e}"
                                ))));
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = done_tx.send(Err(ClientError::Io(e)));
                        return;
                    }
                }
            }
        });

        Ok(StreamFrames {
            frames: frame_rx,
            done: done_rx,
        })
    }
}
