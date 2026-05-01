//! Gateway-owned daemon client adapter.
//!
//! The gateway owns one multiplex-capable daemon client and passes it
//! to workers and command handlers. Lane routing stays focused on
//! mapping platform lanes to daemon session ids.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;

use crate::client::{Client, ClientError};
use crate::extensions::FrontendCapability;

type ClientFactory = Box<
    dyn Fn() -> futures_util::future::BoxFuture<'static, Result<Client, ClientError>> + Send + Sync,
>;

pub struct GatewayDaemonClient {
    client_factory: ClientFactory,
    shared: AsyncMutex<Option<Arc<Client>>>,
}

impl GatewayDaemonClient {
    pub fn new() -> Self {
        Self {
            client_factory: Box::new(|| {
                Box::pin(Client::connect_or_autostart_with_capabilities(
                    FrontendCapability::text_only(),
                ))
            }),
            shared: AsyncMutex::new(None),
        }
    }

    #[cfg(test)]
    pub fn with_client_factory<F>(factory: F) -> Self
    where
        F: Fn() -> futures_util::future::BoxFuture<'static, Result<Client, ClientError>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            client_factory: Box::new(factory),
            shared: AsyncMutex::new(None),
        }
    }

    pub async fn shared_client(&self) -> Result<Arc<Client>, ClientError> {
        let mut g = self.shared.lock().await;
        if let Some(client) = g.as_ref() {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new((self.client_factory)().await?);
        *g = Some(Arc::clone(&client));
        Ok(client)
    }

    pub async fn invalidate(&self) {
        *self.shared.lock().await = None;
    }
}

impl Default for GatewayDaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::server::Server;
    use crate::daemon::state::DaemonState;
    use std::time::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn shared_client_returns_same_arc_across_calls() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&sock, state.clone()).await.unwrap();
        let server_handle = tokio::spawn(server.run());
        tokio::time::sleep(Duration::from_millis(50)).await;

        let sock_path = sock.clone();
        let gateway_client = GatewayDaemonClient::with_client_factory(move || {
            let p = sock_path.clone();
            Box::pin(async move { Client::connect_at(&p).await })
        });

        let c1 = gateway_client.shared_client().await.unwrap();
        let c2 = gateway_client.shared_client().await.unwrap();
        assert!(
            Arc::ptr_eq(&c1, &c2),
            "shared_client must return the same Arc<Client> across calls"
        );

        gateway_client.invalidate().await;
        let c3 = gateway_client.shared_client().await.unwrap();
        assert!(
            !Arc::ptr_eq(&c1, &c3),
            "after invalidate, shared_client must rebuild"
        );

        state.signal_shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(2), server_handle).await;
    }
}
