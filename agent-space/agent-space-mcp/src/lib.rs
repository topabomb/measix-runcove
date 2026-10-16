pub mod admin_api;
pub mod mcp_auth;
pub mod server;
pub mod transport;
pub mod web_publish;

mod guest_http;
pub mod webdav;

pub mod origins;
mod publish_params;

pub mod desktop;
mod rfb;
pub mod standalone;

/// Public routing is a deployment concern; workspace handlers stay shared.
pub(crate) fn space_prefix(state: &agent_engine::state::StateStore, username: &str) -> String {
    if state.is_standalone() {
        String::new()
    } else {
        format!("/u/{username}")
    }
}

pub mod logging;
