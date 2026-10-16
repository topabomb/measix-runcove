use std::collections::HashMap;
use std::sync::Arc;

use agent_engine::auth::extract_bearer_token;
use agent_engine::config::{Config, ServerConfig};
use agent_engine::state::{validate_username, StateStore};
use axum::extract::{OriginalUri, Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tokio_util::sync::{CancellationToken, DropGuard};

const SESSION_ID: &str = "mcp-session-id";

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    username: String,
    agent_space_id: String,
    key_id: String,
}

struct Binding {
    identity: Option<Identity>,
    cancel: CancellationToken,
}

#[derive(Clone)]
pub struct McpAuthState {
    config: Option<Arc<Config>>,
    server: ServerConfig,
    state: Option<Arc<StateStore>>,
    bindings: Arc<RwLock<HashMap<String, Binding>>>,
    _lifetime: Arc<DropGuard>,
}

impl McpAuthState {
    pub fn new(config: Arc<Config>, state: Option<Arc<StateStore>>) -> Self {
        Self::with_authority(config.server.clone(), state, Some(config))
    }

    pub fn with_authority(
        server: ServerConfig,
        state: Option<Arc<StateStore>>,
        config: Option<Arc<Config>>,
    ) -> Self {
        let bindings = Arc::new(RwLock::new(HashMap::<String, Binding>::new()));
        let lifetime = CancellationToken::new();
        if let Some(store) = state.clone() {
            let mut changes = store.subscribe_changes();
            let bindings = bindings.clone();
            let stopped = lifetime.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stopped.cancelled() => break,
                        result = changes.changed() => if result.is_err() { break; },
                    }
                    let mut bindings = bindings.write().await;
                    bindings.retain(|_, binding| {
                        let valid = binding
                            .identity
                            .as_ref()
                            .is_none_or(|identity| current_identity(&store, identity));
                        if !valid {
                            binding.cancel.cancel();
                        }
                        valid
                    });
                }
            });
        }
        Self {
            config,
            server,
            state,
            bindings,
            _lifetime: Arc::new(lifetime.drop_guard()),
        }
    }

    pub fn session_manager(&self) -> crate::transport::BoundSessionManager {
        crate::transport::BoundSessionManager::new(&self.server, self.clone())
    }

    pub(crate) async fn register_transport(&self, id: String, cancel: CancellationToken) {
        self.bindings.write().await.insert(
            id,
            Binding {
                identity: None,
                cancel,
            },
        );
    }

    pub(crate) async fn remove_binding(&self, id: &str) {
        if let Some(binding) = self.bindings.write().await.remove(id) {
            binding.cancel.cancel();
        }
    }

    async fn bind(&self, id: &str, identity: Identity) -> bool {
        let mut bindings = self.bindings.write().await;
        let Some(binding) = bindings.get_mut(id) else {
            return false;
        };
        // The transport may end, or authority may change, while initialize is
        // in flight. Never recreate a binding for a departed transport.
        if binding.cancel.is_cancelled()
            || self
                .state
                .as_ref()
                .is_some_and(|store| !current_identity(store, &identity))
        {
            binding.cancel.cancel();
            bindings.remove(id);
            return false;
        }
        binding.identity = Some(identity);
        true
    }

    fn authenticate(&self, request: &Request) -> Option<Identity> {
        let header = request
            .headers()
            .get(header::AUTHORIZATION)?
            .to_str()
            .ok()?;
        let token = extract_bearer_token(header).ok()?;
        if let Some(state) = &self.state {
            let user = state.authenticate(token).ok()?;
            return Some(Identity {
                username: user.username,
                agent_space_id: user.agent_space_id,
                key_id: user.credential?.key_id,
            });
        }
        let (username, user) = self.config.as_ref()?.get_user_by_token(token)?;
        Some(Identity {
            username: username.clone(),
            agent_space_id: format!("spc_legacy_{}", user.workspace_name),
            key_id: hex::encode(Sha256::digest(token.as_bytes())),
        })
    }
}

/// RMCP handles GET and DELETE at its transport layer, before ServerHandler
/// can recheck a bound identity. Bind the generated protocol session ID at
/// initialization and enforce the same user, space and key on every method.
pub async fn middleware(
    State(state): State<McpAuthState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map(|original| original.0.path())
        .unwrap_or_else(|| request.uri().path());
    let requested_username = if state.state.as_ref().is_some_and(|s| s.is_standalone())
        && matches!(path, "/mcp" | "/mcp/")
    {
        Some("workspace")
    } else {
        mcp_path_username(path)
    };
    let Some(requested_username) = requested_username else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(identity) = state.authenticate(&request) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if identity.username != requested_username {
        return StatusCode::FORBIDDEN.into_response();
    }
    let session_id = request
        .headers()
        .get(SESSION_ID)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if let Some(id) = &session_id {
        if state
            .bindings
            .read()
            .await
            .get(id)
            .and_then(|binding| binding.identity.as_ref())
            != Some(&identity)
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    let method = request.method().clone();
    let response = next.run(request).await;
    if response.status().is_success() {
        if let Some(id) = session_id {
            if method == Method::DELETE {
                state.remove_binding(&id).await;
            }
        } else if method == Method::POST {
            if let Some(id) = response
                .headers()
                .get(SESSION_ID)
                .and_then(|value| value.to_str().ok())
            {
                if !state.bind(id, identity).await {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
            }
        }
    }
    response
}

fn mcp_path_username(path: &str) -> Option<&str> {
    let (username, suffix) = path.strip_prefix("/u/")?.split_once('/')?;
    (suffix == "mcp" && validate_username(username).is_ok()).then_some(username)
}

fn current_identity(store: &StateStore, identity: &Identity) -> bool {
    store.ensure_admission_open().is_ok()
        && store.get(&identity.username).is_ok_and(|user| {
            user.status == agent_engine::state::UserStatus::Active
                && user.agent_space_id == identity.agent_space_id
                && user
                    .credential
                    .is_some_and(|key| key.key_id == identity.key_id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{transport::streamable_http_server::session::SessionManager, ServiceExt};
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, Arc<StateStore>, McpAuthState, Identity) {
        let dir = tempfile::tempdir().unwrap();
        StateStore::init(dir.path()).unwrap();
        let store = Arc::new(StateStore::open(dir.path()).unwrap());
        let (user, _) = store.create_user("alice", "smcp", &["ws"]).unwrap();
        let mut config: Config = toml::from_str(include_str!("../../config.toml")).unwrap();
        config.server.mcp_session_timeout_secs = 1;
        let auth = McpAuthState::new(Arc::new(config), Some(store.clone()));
        let identity = Identity {
            username: user.username,
            agent_space_id: user.agent_space_id,
            key_id: user.credential.unwrap().key_id,
        };
        (dir, store, auth, identity)
    }
    async fn eventually_removed(auth: &McpAuthState, id: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while auth.bindings.read().await.contains_key(id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transport lifecycle must remove its binding");
    }
    #[tokio::test]
    async fn dropped_transport_and_revoked_credential_remove_bindings() {
        let (_dir, store, auth, identity) = setup();
        let manager = auth.session_manager();
        let (id, transport) = manager.create_session().await.unwrap();
        assert!(auth.bind(&id, identity.clone()).await);
        drop(transport);
        eventually_removed(&auth, &id).await;
        assert!(
            !auth.bind(&id, identity.clone()).await,
            "an ended transport cannot be rebound"
        );
        let (id, transport) = manager.create_session().await.unwrap();
        assert!(auth.bind(&id, identity.clone()).await);
        store
            .rotate_token(&identity.username, &identity.agent_space_id)
            .unwrap();
        eventually_removed(&auth, &id).await;
        assert!(transport.cancel_token().is_cancelled());
        assert!(!auth.bind(&id, identity).await);
        drop(transport);
        let (id, transport) = manager.create_session().await.unwrap();
        manager.close_session(&id).await.unwrap();
        assert!(!auth.bindings.read().await.contains_key(id.as_ref()));
        drop(transport);
    }
    struct EmptyServer;
    impl rmcp::ServerHandler for EmptyServer {}
    #[tokio::test]
    async fn rmcp_idle_expiry_removes_binding_without_a_second_timer() {
        let (_dir, _store, auth, identity) = setup();
        let manager = auth.session_manager();
        let (id, transport) = manager.create_session().await.unwrap();
        assert!(auth.bind(&id, identity).await);
        let serving =
            tokio::spawn(
                async move { EmptyServer.serve(transport).await.unwrap().waiting().await },
            );
        let initialize=serde_json::from_value(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"expiry-test","version":"1"}}})).unwrap();
        manager.initialize_session(&id, initialize).await.unwrap();
        manager
            .accept_message(
                &id,
                serde_json::from_value(
                    serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), serving)
            .await
            .expect("RMCP's configured timeout must end the transport")
            .unwrap()
            .unwrap();
        eventually_removed(&auth, &id).await;
        assert!(!manager.has_session(&id).await.unwrap());
    }
}
