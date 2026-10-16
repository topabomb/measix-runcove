//! Runtime-only named web publications. The workspace remains their owner.
use crate::engine::{EngineError, ManagedInstance};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishTarget {
    File(String),
    Directory(String),
    Port(u16),
}
impl PublishTarget {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Directory(_) => "directory",
            Self::Port(_) => "port",
        }
    }
}

/// Shared by Basic and header authentication; constant memory per publication.
pub struct AuthAttempts {
    pub failures: u32,
    pub window: Instant,
    pub blocked_until: Instant,
}

// Intentionally no Debug: credentials must not enter diagnostics.
#[derive(Clone)]
pub struct Publication {
    pub id: String,
    pub name: String,
    pub username: String,
    pub agent_space_id: String,
    pub target: PublishTarget,
    pub port: u16,
    pub password: String,
    pub access_username: String,
    pub helper: Option<ManagedInstance>,
    pub keep_alive_until: SystemTime,
    pub(crate) keep_alive_deadline: Instant,
    pub epoch: Arc<CancellationToken>,
    pub auth_attempts: Arc<Mutex<AuthAttempts>>,
}
impl Publication {
    pub fn new(
        name: String,
        username: String,
        agent_space_id: String,
        target: PublishTarget,
        port: u16,
        helper: Option<ManagedInstance>,
        keep_alive: Duration,
    ) -> Result<Self, EngineError> {
        let now = Instant::now();
        // Rejection sampling avoids modulo bias, including leading zeroes.
        let mut password = String::with_capacity(8);
        while password.len() < 8 {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|error| EngineError::Other(error.to_string()))?;
            for byte in random {
                if byte < 250 && password.len() < 8 {
                    password.push(char::from(b'0' + byte % 10));
                }
            }
        }
        Ok(Self {
            id: format!("pub_{}", uuid::Uuid::new_v4().simple()),
            name,
            username,
            agent_space_id,
            target,
            port,
            access_username: format!("web{}", &password[..2]),
            password: password[2..].to_owned(),
            helper,
            keep_alive_until: SystemTime::now() + keep_alive,
            keep_alive_deadline: now + keep_alive,
            epoch: Arc::new(CancellationToken::new()),
            auth_attempts: Arc::new(Mutex::new(AuthAttempts {
                failures: 0,
                window: now,
                blocked_until: now,
            })),
        })
    }
    pub fn path_prefix(&self) -> String {
        format!("/u/{}/pub/{}/", self.username, self.name)
    }

    /// Called under the workspace publication transaction, before insertion.
    pub fn reserve_username(&mut self, active: &[Publication]) -> Result<(), EngineError> {
        let used: std::collections::HashSet<_> =
            active.iter().map(|p| p.access_username.as_str()).collect();
        let start: usize = self.access_username[3..].parse().expect("generated digits");
        for offset in 0..100 {
            let candidate = format!("web{:02}", (start + offset) % 100);
            if !used.contains(candidate.as_str()) {
                self.access_username = candidate;
                return Ok(());
            }
        }
        Err(EngineError::Other(
            "all 100 publication usernames are in use in this workspace".into(),
        ))
    }

    pub fn extend(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        if deadline > self.keep_alive_deadline {
            self.keep_alive_deadline = deadline;
            self.keep_alive_until = SystemTime::now() + duration;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_credentials_and_username_collision_are_scoped_to_active_items() {
        let mut first = Publication::new(
            "first".into(),
            "alice".into(),
            "spc_a".into(),
            PublishTarget::Port(3000),
            3000,
            None,
            Duration::ZERO,
        )
        .unwrap();
        first.access_username = "web99".into();
        let mut second = first.clone();
        second.reserve_username(&[first]).unwrap();
        assert_eq!(second.access_username, "web00");
        assert_eq!(second.password.len(), 6);
        assert!(second.password.bytes().all(|b| b.is_ascii_digit()));
        assert!(second.id.starts_with("pub_"));
        assert_eq!(second.id.len(), 36);
    }
    #[test]
    fn username_capacity_is_released_with_publication() {
        let mut candidate = Publication::new(
            "next".into(),
            "alice".into(),
            "spc_a".into(),
            PublishTarget::Port(3000),
            3000,
            None,
            Duration::ZERO,
        )
        .unwrap();
        let mut active: Vec<_> = (0..100)
            .map(|i| {
                let mut item = candidate.clone();
                item.access_username = format!("web{i:02}");
                item
            })
            .collect();
        assert!(candidate.reserve_username(&active).is_err());
        active.remove(42);
        candidate.reserve_username(&active).unwrap();
        assert_eq!(candidate.access_username, "web42");
    }
}
