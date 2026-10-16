use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::engine::EngineError;
use crate::session::SessionManager;
use crate::state::{IssuedToken, StateError, StateStore, UserRecord, UserStatus};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("WebDAV is disabled")]
    DavDisabled,
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Engine(#[from] EngineError),
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedUser {
    pub image: crate::image::SpaceImage,
    pub browser: bool,
    pub username: String,
    #[serde(rename = "agentSpaceId")]
    pub agent_space_id: String,
    #[serde(rename = "mcpUrl")]
    pub mcp_url: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserView {
    pub image: crate::image::SpaceImage,
    pub browser: bool,
    pub username: String,
    #[serde(rename = "agentSpaceId")]
    pub agent_space_id: String,
    #[serde(rename = "mcpUrl")]
    pub mcp_url: String,
    pub status: String,
    #[serde(rename = "sandboxName")]
    pub sandbox_name: String,
    #[serde(rename = "ownedVolumeNames")]
    pub owned_volume_names: Vec<String>,
    #[serde(rename = "hasCredential")]
    pub has_credential: bool,
    #[serde(rename = "hasDavCredential")]
    pub has_dav_credential: bool,
    #[serde(rename = "davUrl", skip_serializing_if = "Option::is_none")]
    pub dav_url: Option<String>,
    #[serde(rename = "stopPending")]
    pub stop_pending: bool,
    #[serde(rename = "cleanupError", skip_serializing_if = "Option::is_none")]
    pub cleanup_error: Option<String>,
    #[serde(rename = "workspaceStatus", skip_serializing_if = "Option::is_none")]
    pub workspace_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publications: Option<Vec<PublicationView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<crate::resources::ResourceSummary>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedDavToken {
    pub username: String,
    pub agent_space_id: String,
    pub dav_url: String,
    pub key_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicationView {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone)]
pub struct WorkspaceService {
    state: Arc<StateStore>,
    sessions: Arc<SessionManager>,
    resource_prefix: String,
    public_base_url: String,
    dav_base_url: Option<String>,
    dav_public_port: u16,
    cleanup_gate: Arc<tokio::sync::Mutex<()>>,
    cleanup_scheduled: Arc<Mutex<HashSet<String>>>,
    resources: Arc<crate::resources::ResourceReader>,
    default_image: crate::image::SpaceImage,
}

impl WorkspaceService {
    pub fn new(
        state: Arc<StateStore>,
        sessions: Arc<SessionManager>,
        resource_prefix: impl Into<String>,
        public_base_url: impl Into<String>,
    ) -> Self {
        let default_image = crate::image::SpaceImage::Custom {
            reference: sessions.resource_backend().1.image.clone(),
        };
        Self {
            state,
            sessions,
            resource_prefix: resource_prefix.into(),
            public_base_url: public_base_url.into().trim_end_matches('/').to_string(),
            dav_base_url: None,
            dav_public_port: 8933,
            cleanup_gate: Arc::new(tokio::sync::Mutex::new(())),
            cleanup_scheduled: Arc::new(Mutex::new(HashSet::new())),
            resources: Arc::new(crate::resources::ResourceReader::default()),
            default_image,
        }
    }

    pub fn with_dav_base_url(mut self, base: Option<&str>) -> Self {
        self.dav_base_url = base.map(|value| value.trim_end_matches('/').to_string());
        self
    }

    pub fn with_dav_public_port(mut self, port: u16) -> Self {
        self.dav_public_port = port;
        self
    }

    /// A request-scoped clone shares state and lifecycle owners; it only resolves links.
    pub fn for_request(&self, origin: &url::Url) -> Self {
        let mut service = self.clone();
        if service.public_base_url.is_empty() {
            service.public_base_url = origin.origin().ascii_serialization();
        }
        if service.dav_base_url.as_deref() == Some("") {
            let mut dav = origin.clone();
            dav.set_port(Some(service.dav_public_port))
                .expect("HTTP origin");
            service.dav_base_url = Some(dav.origin().ascii_serialization());
        }
        service
    }

    pub fn with_default_image(mut self, image: crate::image::SpaceImage) -> Self {
        self.default_image = image;
        self
    }

    pub fn state(&self) -> &StateStore {
        &self.state
    }

    pub fn sessions(&self) -> &SessionManager {
        &self.sessions
    }

    pub fn create_user(&self, username: &str) -> Result<CreatedUser, ServiceError> {
        self.create_user_with_image(username, self.default_image.clone())
    }

    pub fn create_user_with_image(
        &self,
        username: &str,
        image: crate::image::SpaceImage,
    ) -> Result<CreatedUser, ServiceError> {
        let (user, token) =
            self.state
                .create_user_with_image(username, &self.resource_prefix, &["ws"], image)?;
        self.sessions
            .register_image(&user.sandbox_name, user.image.as_ref().unwrap().reference())?;
        Ok(created(user, token, &self.public_base_url))
    }

    pub fn list_users(&self) -> Vec<UserView> {
        self.state
            .list()
            .into_iter()
            .map(|user| {
                view_without_runtime(user, &self.public_base_url, self.dav_base_url.as_deref())
            })
            .collect()
    }

    pub async fn get_user(&self, username: &str) -> Result<UserView, ServiceError> {
        self.user_detail(username, true).await
    }

    async fn user_detail(
        &self,
        username: &str,
        include_resources: bool,
    ) -> Result<UserView, ServiceError> {
        let user = self.state.get(username)?;
        let mut view = view_without_runtime(
            user.clone(),
            &self.public_base_url,
            self.dav_base_url.as_deref(),
        );
        view.publications = self.publication_summary(&user.sandbox_name).await;
        if !include_resources {
            view.workspace_status = Some(
                match self.sessions.sessions_status(&user.sandbox_name).await {
                    Ok(status) => format!("{status:?}").to_ascii_lowercase(),
                    Err(_) => "unknown".into(),
                },
            );
            return Ok(view);
        }
        let (engine, defaults) = self.sessions.resource_backend();
        let mut summary = self
            .resources
            .read(
                engine,
                &user.agent_space_id,
                &user.sandbox_name,
                &user.owned_volume_names,
                defaults,
                user.status == UserStatus::Active,
            )
            .await;
        // Never return or retain a late result for a deleted/recreated account.
        let current = self.state.get(username);
        match current {
            Ok(current) if current.agent_space_id == user.agent_space_id => {
                if current.status != user.status {
                    view = view_without_runtime(
                        current.clone(),
                        &self.public_base_url,
                        self.dav_base_url.as_deref(),
                    );
                    summary.memory = crate::resources::Observation::unavailable("account_changed");
                    if summary.disk.value.is_some() {
                        summary.disk.status = "historical";
                        summary.disk.reason = Some("account_changed".into());
                    }
                }
                if current.status == UserStatus::Deleting {
                    self.resources.remove(&user.agent_space_id);
                }
            }
            Ok(_) => {
                self.resources.remove(&user.agent_space_id);
                return Err(StateError::SpaceMismatch.into());
            }
            Err(error) => {
                self.resources.remove(&user.agent_space_id);
                return Err(error.into());
            }
        }
        view.workspace_status = Some(
            summary
                .runtime
                .value
                .clone()
                .unwrap_or_else(|| "unknown".into()),
        );
        view.resources = Some(summary);
        Ok(view)
    }

    pub async fn set_enabled(
        &self,
        username: &str,
        agent_space_id: &str,
        enabled: bool,
    ) -> Result<UserView, ServiceError> {
        let user = self.state.set_enabled(username, agent_space_id, enabled)?;
        if !enabled {
            // Cancel execution before waiting behind unrelated physical cleanup,
            // but never cancel a fresh owner using stale disabled state.
            self.sessions
                .cancel_workspace_if(&user.sandbox_name, || {
                    self.state.get(username).is_ok_and(|current| {
                        current.agent_space_id == agent_space_id
                            && current.status != UserStatus::Active
                    })
                })
                .await;
            self.stop_disabled_user(&user.username, &user.agent_space_id)
                .await?;
        }
        self.user_detail(username, false).await
    }

    pub fn rotate_token(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<IssuedToken, ServiceError> {
        Ok(self.state.rotate_token(username, agent_space_id)?)
    }

    pub fn rotate_dav_token(
        &self,
        username: &str,
        agent_space_id: &str,
        token: Option<&str>,
    ) -> Result<IssuedDavToken, ServiceError> {
        // Resolve the account/space first so missing and stale identities retain
        // the same 404/409 contract even while DAV is globally disabled.
        let user = self.state.get(username)?;
        if user.agent_space_id != agent_space_id {
            return Err(StateError::SpaceMismatch.into());
        }
        let base = self
            .dav_base_url
            .as_deref()
            .ok_or(ServiceError::DavDisabled)?;
        let issued = match token {
            Some(token) => self.state.set_dav_token(username, agent_space_id, token)?,
            None => self.state.rotate_dav_token(username, agent_space_id)?,
        };
        Ok(IssuedDavToken {
            username: username.into(),
            agent_space_id: agent_space_id.into(),
            dav_url: format!("{base}/u/{username}/dav/"),
            key_id: issued.key_id,
            token: issued.token,
        })
    }
    pub fn revoke_dav_token(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<(), ServiceError> {
        Ok(self.state.revoke_dav_token(username, agent_space_id)?)
    }

    /// Commit deleting before touching the engine. The HTTP response only
    /// acknowledges the durable intent; cleanup continues in the background.
    pub async fn delete_user(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<bool, ServiceError> {
        let user = match self.state.get(username) {
            Ok(user) => user,
            Err(StateError::NotFound(_)) => return Ok(true),
            Err(error) => return Err(error.into()),
        };
        if user.agent_space_id != agent_space_id {
            return Err(StateError::SpaceMismatch.into());
        }
        let user = self.state.begin_delete(username, agent_space_id)?;
        self.resources.remove(agent_space_id);
        self.sessions.cancel_workspace(&user.sandbox_name).await;
        if self
            .cleanup_scheduled
            .lock()
            .expect("cleanup jobs mutex")
            .insert(user.agent_space_id.clone())
        {
            let service = self.clone();
            tokio::spawn(async move {
                if let Err(error) = service
                    .cleanup_deleting_user(&user.username, &user.agent_space_id)
                    .await
                {
                    tracing::error!(user = %user.username, error = %error, "workspace deletion remains pending");
                }
                service
                    .cleanup_scheduled
                    .lock()
                    .expect("cleanup jobs mutex")
                    .remove(&user.agent_space_id);
            });
        }
        Ok(false)
    }

    pub async fn retry_pending_stops(&self) -> Result<(), ServiceError> {
        for user in self.state.pending_stops() {
            self.stop_disabled_user(&user.username, &user.agent_space_id)
                .await?;
        }
        Ok(())
    }

    async fn stop_disabled_user(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<(), ServiceError> {
        // Share the existing cleanup gate with deletion. A pending-stop list is
        // only a snapshot: another request may have finished stopping and then
        // re-enabled this account while an earlier user was being processed.
        let _gate = self.cleanup_gate.lock().await;
        let user = match self.state.get(username) {
            Ok(user)
                if user.agent_space_id == agent_space_id
                    && user.status == UserStatus::Disabled
                    && user.cleanup.is_some() =>
            {
                user
            }
            Ok(_) | Err(StateError::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        match self.sessions.revoke_workspace(&user.sandbox_name).await {
            Ok(()) | Err(EngineError::NotFound(_)) => {
                self.state.clear_stop_pending(username, agent_space_id)?;
            }
            Err(error) => {
                self.state
                    .note_cleanup_error(username, agent_space_id, &error.to_string())?;
            }
        }
        Ok(())
    }

    pub async fn retry_deletions(&self) -> Result<(), ServiceError> {
        for user in self.state.deleting_users() {
            if let Err(error) = self
                .cleanup_deleting_user(&user.username, &user.agent_space_id)
                .await
            {
                tracing::error!(user = %user.username, error = %error, "workspace deletion retry remains pending");
            }
        }
        Ok(())
    }

    async fn cleanup_deleting_user(
        &self,
        username: &str,
        agent_space_id: &str,
    ) -> Result<(), ServiceError> {
        let _gate = self.cleanup_gate.lock().await;
        let user = match self.state.get(username) {
            Ok(user)
                if user.agent_space_id == agent_space_id && user.status == UserStatus::Deleting =>
            {
                user
            }
            Ok(_) | Err(StateError::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        match self
            .sessions
            .destroy_recorded_resources(&user.sandbox_name, &user.owned_volume_names)
            .await
        {
            Ok(()) => {
                self.state.finish_delete(username, agent_space_id)?;
                self.resources.remove(agent_space_id);
                Ok(())
            }
            Err(error) => {
                self.state
                    .note_cleanup_error(username, agent_space_id, &error.to_string())?;
                Err(error.into())
            }
        }
    }

    pub async fn readiness(&self) -> Result<usize, ServiceError> {
        self.state.check_readable()?;
        self.sessions.check_backend().await?;
        Ok(self.sessions.active_space_count())
    }

    pub async fn report_unknown(&self) -> Result<Vec<String>, ServiceError> {
        let (sandboxes, volumes) = self.state.registered_resources();
        Ok(self
            .sessions
            .report_unregistered_resources(&sandboxes, &volumes)
            .await?)
    }

    async fn publication_summary(&self, sandbox_name: &str) -> Option<Vec<PublicationView>> {
        let session = self.sessions.find_existing_session(sandbox_name).await?;
        Some(
            session
                .publications()
                .iter()
                .map(|item| PublicationView {
                    name: item.name.clone(),
                    kind: item.target.kind().into(),
                })
                .collect(),
        )
    }
}

fn created(user: UserRecord, token: IssuedToken, public_base_url: &str) -> CreatedUser {
    CreatedUser {
        browser: user.image.as_ref().unwrap().browser(),
        image: user.image.clone().unwrap(),
        mcp_url: format!("{public_base_url}/u/{}/mcp", user.username),
        username: user.username,
        agent_space_id: user.agent_space_id,
        token: token.token,
    }
}

fn view_without_runtime(
    user: UserRecord,
    public_base_url: &str,
    dav_base_url: Option<&str>,
) -> UserView {
    let stop_pending = user.status == UserStatus::Disabled && user.cleanup.is_some();
    UserView {
        browser: user.image.as_ref().unwrap().browser(),
        image: user.image.clone().unwrap(),
        dav_url: dav_base_url.map(|base| format!("{base}/u/{}/dav/", user.username)),
        has_dav_credential: user.dav_credential.is_some(),
        mcp_url: format!("{public_base_url}/u/{}/mcp", user.username),
        username: user.username,
        agent_space_id: user.agent_space_id,
        status: match user.status {
            UserStatus::Active => "active",
            UserStatus::Disabled => "disabled",
            UserStatus::Deleting => "deleting",
        }
        .into(),
        sandbox_name: user.sandbox_name,
        owned_volume_names: user.owned_volume_names,
        has_credential: user.credential.is_some(),
        stop_pending,
        cleanup_error: user.cleanup.and_then(|cleanup| cleanup.last_error),
        workspace_status: None,
        publications: None,
        resources: None,
    }
}
