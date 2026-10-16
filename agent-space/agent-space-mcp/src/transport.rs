use agent_engine::config::ServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use std::time::Duration;

/// Protocol sessions have a separate lifetime from their sleeping workspace.
pub fn session_manager(config: &ServerConfig) -> LocalSessionManager {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = match config.mcp_session_timeout_secs {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    };
    sessions
}

use crate::mcp_auth::McpAuthState;
use futures::Stream;
use rmcp::{
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::streamable_http_server::session::{ServerSseMessage, SessionId, SessionManager},
};
use std::sync::Arc;

/// Delegates protocol behavior to RMCP, while tying auth bindings to the actual
/// transport lifetime. The worker's existing keep-alive is the only expiry clock.
pub struct BoundSessionManager {
    inner: Arc<LocalSessionManager>,
    auth: McpAuthState,
}
impl BoundSessionManager {
    pub(crate) fn new(config: &ServerConfig, auth: McpAuthState) -> Self {
        Self {
            inner: Arc::new(session_manager(config)),
            auth,
        }
    }
}
impl SessionManager for BoundSessionManager {
    type Error = <LocalSessionManager as SessionManager>::Error;
    type Transport = <LocalSessionManager as SessionManager>::Transport;
    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let (id, transport) = self.inner.create_session().await?;
        let cancel = transport.cancel_token();
        self.auth
            .register_transport(id.to_string(), cancel.clone())
            .await;
        let auth = self.auth.clone();
        let inner = self.inner.clone();
        let ended = id.clone();
        tokio::spawn(async move {
            cancel.cancelled().await;
            auth.remove_binding(&ended).await;
            let _ = inner.close_session(&ended).await;
        });
        Ok((id, transport))
    }
    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        self.inner.initialize_session(id, message).await
    }
    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        self.inner.has_session(id).await
    }
    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.auth.remove_binding(id).await;
        self.inner.close_session(id).await
    }
    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner.create_stream(id, message).await
    }
    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.inner.accept_message(id, message).await
    }
    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner.create_standalone_stream(id).await
    }
    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner.resume(id, last_event_id).await
    }
}
