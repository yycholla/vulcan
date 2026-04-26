//! Per-chat serial dispatch with parallelism across chats.
//!
//! `LaneRouter` spawns one worker task per `LaneKey`. Messages dispatched to
//! the same key run strictly in order; messages on different keys run on
//! independent tasks and so make use of the multi-thread runtime.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Notify, RwLock, mpsc};

#[derive(Clone, Eq, Hash, PartialEq, Debug)]
pub struct LaneKey {
    pub platform: String,
    pub chat_id: String,
}

pub trait Handler<M>: Send + Sync + 'static {
    fn handle(
        &self,
        lane: LaneKey,
        msg: M,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

struct ClosureHandler<F>(F);

impl<F, Fut, M> Handler<M> for ClosureHandler<F>
where
    F: Fn(LaneKey, M) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
    M: Send + 'static,
{
    fn handle(&self, lane: LaneKey, msg: M) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin((self.0)(lane, msg))
    }
}

pub fn from_closure<F, Fut, M>(f: F) -> impl Handler<M>
where
    F: Fn(LaneKey, M) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
    M: Send + 'static,
{
    ClosureHandler(f)
}

const DEFAULT_CHANNEL_CAPACITY: usize = 32;

pub struct LaneRouter<M> {
    inner: Arc<RwLock<HashMap<LaneKey, mpsc::Sender<M>>>>,
    handler: Arc<dyn Handler<M>>,
    pending: Arc<AtomicUsize>,
    completed_notify: Arc<Notify>,
    channel_capacity: usize,
}

impl<M: Send + 'static> LaneRouter<M> {
    pub fn new<H>(handler: H) -> Self
    where
        H: Handler<M>,
    {
        Self::with_capacity(handler, DEFAULT_CHANNEL_CAPACITY)
    }

    pub fn with_capacity<H>(handler: H, channel_capacity: usize) -> Self
    where
        H: Handler<M>,
    {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            handler: Arc::new(handler),
            pending: Arc::new(AtomicUsize::new(0)),
            completed_notify: Arc::new(Notify::new()),
            channel_capacity,
        }
    }

    /// Send `msg` to the worker owning `lane`. Spawns the worker on first use.
    pub async fn dispatch(&self, lane: LaneKey, msg: M) {
        // Increment before send so drain() can never miss an in-flight message.
        self.pending.fetch_add(1, Ordering::SeqCst);

        {
            let map = self.inner.read().await;
            if let Some(tx) = map.get(&lane) {
                let tx = tx.clone();
                drop(map);
                if tx.send(msg).await.is_err() {
                    // Worker is gone — keep the counter consistent.
                    self.decrement_pending();
                }
                return;
            }
        }

        let mut map = self.inner.write().await;
        // Re-check after acquiring the write lock — another dispatcher may
        // have spawned the worker between our read and write.
        if let Some(tx) = map.get(&lane) {
            let tx = tx.clone();
            drop(map);
            if tx.send(msg).await.is_err() {
                self.decrement_pending();
            }
            return;
        }

        let (tx, mut rx) = mpsc::channel::<M>(self.channel_capacity);
        let handler = Arc::clone(&self.handler);
        let pending = Arc::clone(&self.pending);
        let notify = Arc::clone(&self.completed_notify);
        let lane_for_worker = lane.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                handler.handle(lane_for_worker.clone(), msg).await;
                let prev = pending.fetch_sub(1, Ordering::SeqCst);
                if prev == 1 {
                    notify.notify_waiters();
                }
            }
        });
        map.insert(lane.clone(), tx.clone());
        drop(map);

        if tx.send(msg).await.is_err() {
            self.decrement_pending();
        }
    }

    fn decrement_pending(&self) {
        let prev = self.pending.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            self.completed_notify.notify_waiters();
        }
    }

    /// Wait until every dispatched message has been handled.
    pub async fn drain(&self) {
        loop {
            if self.pending.load(Ordering::SeqCst) == 0 {
                return;
            }
            let notified = self.completed_notify.notified();
            // Re-check after registering the waiter to close the race where a
            // worker decrements between our load and the await.
            if self.pending.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use tokio::sync::Mutex;

    #[derive(Debug)]
    struct TestMsg {
        seq: u32,
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lanes_serial_within_parallel_across() {
        let observed: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let observed_clone = observed.clone();
        let handler = from_closure(move |lane: LaneKey, msg: TestMsg| {
            let observed = observed_clone.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                observed.lock().await.push((lane.chat_id.clone(), msg.seq));
            }
        });
        let router: LaneRouter<TestMsg> = LaneRouter::new(handler);

        let a = LaneKey {
            platform: "loop".into(),
            chat_id: "A".into(),
        };
        let b = LaneKey {
            platform: "loop".into(),
            chat_id: "B".into(),
        };
        let started = Instant::now();
        router.dispatch(a.clone(), TestMsg { seq: 1 }).await;
        router.dispatch(a.clone(), TestMsg { seq: 2 }).await;
        router.dispatch(b.clone(), TestMsg { seq: 1 }).await;
        router.drain().await;
        let elapsed = started.elapsed();

        // Lane A's two msgs serialize -> at least 100ms. Lane B runs in parallel
        // -> total elapsed should be < 140ms (would be 150ms if all three serialized).
        assert!(elapsed >= Duration::from_millis(100), "got {elapsed:?}");
        assert!(elapsed < Duration::from_millis(140), "got {elapsed:?}");

        let v = observed.lock().await.clone();
        let a_seqs: Vec<u32> = v
            .iter()
            .filter(|(c, _)| c == "A")
            .map(|(_, s)| *s)
            .collect();
        assert_eq!(a_seqs, vec![1, 2]);
    }

    #[tokio::test]
    async fn drain_returns_immediately_with_no_pending() {
        let handler = from_closure(|_: LaneKey, _: ()| async {});
        let router: LaneRouter<()> = LaneRouter::new(handler);
        let started = Instant::now();
        router.drain().await;
        assert!(started.elapsed() < Duration::from_millis(20));
    }

    #[tokio::test]
    async fn drain_waits_for_in_flight() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter2 = counter.clone();
        let handler = from_closure(move |_: LaneKey, _: ()| {
            let counter = counter2.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        let router: LaneRouter<()> = LaneRouter::new(handler);
        let lane = LaneKey {
            platform: "p".into(),
            chat_id: "c".into(),
        };
        for _ in 0..3 {
            router.dispatch(lane.clone(), ()).await;
        }
        router.drain().await;
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
}
