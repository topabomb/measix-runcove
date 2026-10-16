use std::sync::Arc;

use crate::origins::RequestOrigin;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use agent_engine::auth::extract_bearer_token;
use agent_engine::state::StateError;
use agent_engine::workspace_service::{ServiceError, WorkspaceService};

#[derive(Clone)]
pub struct AdminState {
    pub service: Arc<WorkspaceService>,
    pub admin_token: String,
}

pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/v1/status", get(status))
        .route("/admin/v1/users", post(create_user).get(list_users))
        .route(
            "/admin/v1/users/{username}",
            get(get_user).patch(set_enabled).delete(delete_user),
        )
        .route("/admin/v1/users/{username}/token", post(rotate_token))
        .route(
            "/admin/v1/users/{username}/dav-token",
            post(rotate_dav_token).delete(revoke_dav_token),
        )
        .with_state(state)
}

#[derive(Deserialize)]
struct UsernameBody {
    username: String,
    #[serde(default)]
    image: Option<agent_engine::image::ImageSelection>,
}

#[derive(Deserialize)]
struct EnableBody {
    #[serde(rename = "agentSpaceId")]
    agent_space_id: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct SpaceBody {
    #[serde(rename = "agentSpaceId")]
    agent_space_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DavTokenBody {
    #[serde(rename = "agentSpaceId")]
    agent_space_id: String,
    // A present null is invalid, rather than silently requesting rotation.
    #[serde(default, deserialize_with = "deserialize_dav_token")]
    token: Option<String>,
}
fn deserialize_dav_token<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

#[derive(Deserialize)]
struct DeleteQuery {
    #[serde(rename = "agentSpaceId")]
    agent_space_id: String,
}

#[derive(Serialize)]
struct TokenResponse {
    token: String,
    #[serde(rename = "keyId")]
    key_id: String,
}

async fn create_user(
    State(state): State<AdminState>,
    headers: HeaderMap,
    origin: Option<Extension<RequestOrigin>>,
    Json(body): Json<UsernameBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let service = origin.map(|Extension(origin)| state.service.for_request(&origin.0));
    let service = service.as_ref().unwrap_or(&state.service);
    let result = match body.image {
        Some(image) => {
            let image = image.resolve().map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"invalid_image","message":error.to_string()})),
                )
            })?;
            service.create_user_with_image(&body.username, image)
        }
        None => service.create_user(&body.username),
    };
    match result {
        Ok(created) => Ok((
            StatusCode::CREATED,
            Json(serde_json::to_value(created).unwrap()),
        )),
        Err(error) => Err(map_error(error)),
    }
}

async fn status(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    match state.service.readiness().await {
        Ok(active) => Ok(Json(json!({"ready": true, "activeSpaces": active}))),
        Err(error) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready": false, "error": error.to_string()})),
        )),
    }
}

async fn list_users(
    State(state): State<AdminState>,
    headers: HeaderMap,
    origin: Option<Extension<RequestOrigin>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let service = origin.map(|Extension(origin)| state.service.for_request(&origin.0));
    let service = service.as_ref().unwrap_or(&state.service);
    Ok(Json(json!({ "users": service.list_users() })))
}

async fn get_user(
    State(state): State<AdminState>,
    headers: HeaderMap,
    origin: Option<Extension<RequestOrigin>>,
    Path(username): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let service = origin.map(|Extension(origin)| state.service.for_request(&origin.0));
    let service = service.as_ref().unwrap_or(&state.service);
    match service.get_user(&username).await {
        Ok(user) => Ok(Json(serde_json::to_value(user).unwrap())),
        Err(error) => Err(map_error(error)),
    }
}

async fn set_enabled(
    State(state): State<AdminState>,
    headers: HeaderMap,
    origin: Option<Extension<RequestOrigin>>,
    Path(username): Path<String>,
    Json(body): Json<EnableBody>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let service = origin.map(|Extension(origin)| state.service.for_request(&origin.0));
    let service = service.as_ref().unwrap_or(&state.service);
    match service
        .set_enabled(&username, &body.agent_space_id, body.enabled)
        .await
    {
        Ok(user) if user.status == "disabled" && user.cleanup_error.is_some() => Err((
            StatusCode::ACCEPTED,
            Json(json!({"status": "disabled", "stop": "pending"})),
        )),
        Ok(user) => Ok((StatusCode::OK, Json(serde_json::to_value(user).unwrap()))),
        Err(error) => Err(map_error(error)),
    }
}

async fn rotate_token(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(body): Json<SpaceBody>,
) -> Result<Json<TokenResponse>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    match state.service.rotate_token(&username, &body.agent_space_id) {
        Ok(issued) => Ok(Json(TokenResponse {
            token: issued.token,
            key_id: issued.key_id,
        })),
        Err(error) => Err(map_error(error)),
    }
}

async fn rotate_dav_token(
    State(state): State<AdminState>,
    headers: HeaderMap,
    origin: Option<Extension<RequestOrigin>>,
    Path(username): Path<String>,
    body: Result<Json<DavTokenBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<agent_engine::workspace_service::IssuedDavToken>, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    let service = origin.map(|Extension(origin)| state.service.for_request(&origin.0));
    let service = service.as_ref().unwrap_or(&state.service);
    let Json(body) = body.map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_request"})),
        )
    })?;
    service
        .rotate_dav_token(&username, &body.agent_space_id, body.token.as_deref())
        .map(Json)
        .map_err(map_error)
}
async fn revoke_dav_token(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    state
        .service
        .revoke_dav_token(&username, &query.agent_space_id)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(map_error)
}

async fn delete_user(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    authorize(&state, &headers)?;
    match state
        .service
        .delete_user(&username, &query.agent_space_id)
        .await
    {
        Ok(false) | Err(ServiceError::Engine(_)) => Ok(StatusCode::ACCEPTED),
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Err(error) => Err(map_error(error)),
    }
}

fn authorize(state: &AdminState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    let header = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            )
        })?;
    let token = extract_bearer_token(header).map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
    })?;
    if !constant_eq(token, &state.admin_token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        ));
    }
    Ok(())
}

fn map_error(error: ServiceError) -> (StatusCode, Json<Value>) {
    let (status, code) = match &error {
        ServiceError::DavDisabled => (StatusCode::CONFLICT, "dav_disabled"),
        ServiceError::State(StateError::AlreadyExists(_)) => (StatusCode::CONFLICT, "conflict"),
        ServiceError::State(StateError::SpaceMismatch) => (StatusCode::CONFLICT, "space_mismatch"),
        ServiceError::State(StateError::NotFound(_)) => (StatusCode::NOT_FOUND, "not_found"),
        ServiceError::State(StateError::InvalidDavToken) => {
            (StatusCode::BAD_REQUEST, "invalid_dav_token")
        }
        ServiceError::State(StateError::InvalidUsername) => {
            (StatusCode::BAD_REQUEST, "invalid_username")
        }
        ServiceError::State(StateError::Deleting(_)) => (StatusCode::CONFLICT, "deleting"),
        ServiceError::State(StateError::Disabled(_)) => (StatusCode::CONFLICT, "disabled"),
        ServiceError::State(StateError::StopPending(_)) => (StatusCode::CONFLICT, "stop_pending"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    (
        status,
        Json(json!({"error": code, "message": error.to_string()})),
    )
}

fn constant_eq(left: &str, right: &str) -> bool {
    let left = Sha256::digest(left.as_bytes());
    let right = Sha256::digest(right.as_bytes());
    bool::from(left.as_slice().ct_eq(right.as_slice()))
}
