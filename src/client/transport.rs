//! UnixStream transport: framing + request/response correlation.
//!
//! Slice 5: one read task per [`Transport`]. The reader routes
//! [`Response`] and [`StreamFrame`]s to per-request channels keyed by
//! the wire `id`, so the same [`Transport`] can serve multiple
//! in-flight calls concurrently. Push frames (`id == None`) are
//! delivered through a single broadcast channel attached to the
//! transport.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::daemon::protocol::{Request, Response, StreamFrame, read_frame_bytes, write_request};

use super::errors::{ClientError, ClientResult};

/// A handle returned by [`Transport::call_stream`]. The caller drains
/// [`StreamFrame`]s via `frames.recv().await` and finally awaits
/// `done` for the terminal [`Response`].
#[allow(dead_code)]
pub struct StreamFrames {
    pub frames: mpsc::Receiver<StreamFrame>,
    pub done: oneshot::Receiver<ClientResult<Response>>,
}

type PendingMap = HashMap<String, oneshot::Sender<ClientResult<Response>>>;
type StreamMap = HashMap<String, mpsc::Sender<StreamFrame>>;

pub struct Transport {
    write_half: Arc<Mutex<OwnedWriteHalf>>,
    pending: Arc<Mutex<PendingMap>>,
    streams: Arc<Mutex<StreamMap>>,
    /// Broadcast for `id == None` server push frames (config_reloaded,
    /// session_evicted, etc.). Subscribers register via
    /// [`Transport::take_push_receiver`]; only one taker is supported in
    /// Slice 5.
    push_tx: mpsc::Sender<StreamFrame>,
    push_rx: Mutex<Option<mpsc::Receiver<StreamFrame>>>,
    /// Background read loop. Dropped (cancelled) when the transport
    /// drops, which closes the socket and wakes every pending
    /// `oneshot::Receiver` with `Err(disconnected)`.
    _read_task: JoinHandle<()>,
}

impl Transport {
    /// Connect to the daemon socket at `path`. Spawns one reader task
    /// for the lifetime of the [`Transport`] that demultiplexes
    /// inbound frames by request id.
    pub async fn connect(path: &Path) -> ClientResult<Self> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(ClientError::ConnectionRefused)?;
        let (read_half, write_half) = stream.into_split();
        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let streams: Arc<Mutex<StreamMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (push_tx, push_rx) = mpsc::channel::<StreamFrame>(16);
        let read_task = tokio::spawn(reader_loop(
            read_half,
            Arc::clone(&pending),
            Arc::clone(&streams),
            push_tx.clone(),
        ));
        Ok(Self {
            write_half: Arc::new(Mutex::new(write_half)),
            pending,
            streams,
            push_tx,
            push_rx: Mutex::new(Some(push_rx)),
            _read_task: read_task,
        })
    }

    /// Send `req`, await the matching response frame routed by id.
    /// Multiple concurrent callers can invoke this on the same
    /// [`Transport`] — the writer Mutex serializes the framed write,
    /// the reader demultiplexes the response.
    pub async fn call(&self, req: Request) -> ClientResult<Response> {
        let id = req.id.clone();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        if let Err(e) = self.write_one(&req).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(ClientError::Protocol(
                "daemon connection closed before response".into(),
            )),
        }
    }

    /// Send a streaming request and return a handle for incremental
    /// frames + a final response. Slice 5: no socket-stealing — the
    /// shared reader routes by id so other in-flight calls keep
    /// working in parallel.
    #[allow(dead_code)]
    pub async fn call_stream(&self, req: Request) -> ClientResult<StreamFrames> {
        let id = req.id.clone();
        let (frame_tx, frame_rx) = mpsc::channel::<StreamFrame>(16);
        let (done_tx, done_rx) = oneshot::channel::<ClientResult<Response>>();
        {
            let mut streams = self.streams.lock().await;
            streams.insert(id.clone(), frame_tx);
        }
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), done_tx);
        }
        if let Err(e) = self.write_one(&req).await {
            self.streams.lock().await.remove(&id);
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        Ok(StreamFrames {
            frames: frame_rx,
            done: done_rx,
        })
    }

    /// Take ownership of the push-frame receiver. Returns `None` on
    /// the second call. Daemon push frames (`id == None`) flow through
    /// this channel; consumers that don't need them simply never
    /// take it.
    #[allow(dead_code)]
    pub async fn take_push_receiver(&self) -> Option<mpsc::Receiver<StreamFrame>> {
        self.push_rx.lock().await.take()
    }

    async fn write_one(&self, req: &Request) -> ClientResult<()> {
        let mut wh = self.write_half.lock().await;
        write_request(&mut *wh, req).await?;
        wh.flush().await?;
        Ok(())
    }
}

async fn reader_loop(
    mut read_half: OwnedReadHalf,
    pending: Arc<Mutex<PendingMap>>,
    streams: Arc<Mutex<StreamMap>>,
    push_tx: mpsc::Sender<StreamFrame>,
) {
    loop {
        match read_frame_bytes(&mut read_half).await {
            Ok(body) => {
                // Try StreamFrame first because Response's optional
                // `result` / `error` fields would silently swallow a
                // StreamFrame body otherwise. StreamFrame requires
                // `stream` + `data`, so a Response body fails to
                // deserialize as StreamFrame.
                if let Ok(frame) = serde_json::from_slice::<StreamFrame>(&body) {
                    match &frame.id {
                        Some(id) => {
                            if let Some(tx) = streams.lock().await.get(id) {
                                let _ = tx.send(frame).await;
                            }
                        }
                        None => {
                            // Push frame — best-effort delivery to the
                            // optional consumer.
                            let _ = push_tx.send(frame).await;
                        }
                    }
                    continue;
                }
                if let Ok(resp) = serde_json::from_slice::<Response>(&body) {
                    let id = resp.id.clone();
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(Ok(resp));
                    }
                    streams.lock().await.remove(&id);
                    continue;
                }
                tracing::warn!(
                    "client transport: failed to decode {} bytes as Response or StreamFrame",
                    body.len()
                );
            }
            Err(e) => {
                tracing::debug!("client transport: read loop ended: {e}");
                let drained: PendingMap = std::mem::take(&mut *pending.lock().await);
                for (_, tx) in drained {
                    let _ = tx.send(Err(ClientError::Io(std::io::Error::other(
                        "daemon connection closed",
                    ))));
                }
                streams.lock().await.clear();
                return;
            }
        }
    }
}
