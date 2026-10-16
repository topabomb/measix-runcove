//! One browser-operation slot for a desktop space. The request/worker owns
//! the guard, so a dropped HTTP waiter cannot release a still-running operation.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Mode {
    Ai,
    Human,
    Transition,
    Blocked,
}
struct Active {
    id: String,
    cancel: CancellationToken,
}
struct State {
    mode: Mode,
    epoch: u64,
    active: Option<Active>,
    owner: Option<String>,
    deadline: Instant,
    error: Option<String>,
}
pub struct ControlGate {
    state: Mutex<State>,
    changed: Notify,
}
pub struct Operation {
    gate: Arc<ControlGate>,
    id: String,
    pub cancel: CancellationToken,
    epoch: u64,
}
/// A cancelled HTTP waiter must not leave the gate in a transition forever.
/// The active operation still owns its slot and decides whether cleanup failed.
struct Handoff<'a> {
    gate: &'a ControlGate,
    epoch: u64,
}
impl Drop for Handoff<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        if state.mode == Mode::Transition && state.epoch == self.epoch {
            state.mode = if state.error.is_some() {
                Mode::Blocked
            } else {
                Mode::Ai
            };
            state.epoch += 1;
            drop(state);
            self.gate.changed.notify_waiters();
        }
    }
}
#[derive(serde::Serialize)]
pub struct ControlView {
    pub control: Mode,
    pub human_remaining_secs: u64,
    pub can_input: bool,
    pub control_error: Option<String>,
    pub human_idle_secs: u64,
}
impl Default for ControlGate {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                mode: Mode::Ai,
                epoch: 0,
                active: None,
                owner: None,
                deadline: Instant::now(),
                error: None,
            }),
            changed: Notify::new(),
        }
    }
}
impl ControlGate {
    fn expire(state: &mut State) {
        if state.mode == Mode::Human && Instant::now() >= state.deadline {
            state.mode = Mode::Ai;
            state.owner = None;
            state.epoch += 1;
        }
    }
    pub(crate) fn block(&self) {
        let mut state = self.state.lock().unwrap();
        state.error = Some("CLEANUP_UNCONFIRMED".into());
        state.mode = Mode::Blocked;
        state.owner = None;
        state.epoch += 1;
        if let Some(active) = &state.active {
            active.cancel.cancel();
        }
        drop(state);
        self.changed.notify_waiters();
    }
    pub fn begin(self: &Arc<Self>) -> Result<Operation, &'static str> {
        let mut state = self.state.lock().unwrap();
        Self::expire(&mut state);
        if state.mode == Mode::Blocked {
            return Err("CLEANUP_UNCONFIRMED");
        }
        if state.mode != Mode::Ai {
            return Err("HUMAN_CONTROL");
        }
        if state.active.is_some() {
            return Err("BUSY");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        state.active = Some(Active {
            id: id.clone(),
            cancel: cancel.clone(),
        });
        Ok(Operation {
            gate: self.clone(),
            id,
            cancel,
            epoch: state.epoch,
        })
    }
    pub fn view(&self, owner: &str) -> ControlView {
        let mut state = self.state.lock().unwrap();
        Self::expire(&mut state);
        ControlView {
            control: state.mode,
            human_remaining_secs: if state.mode == Mode::Human {
                state
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs()
            } else {
                0
            },
            can_input: state.mode == Mode::Human && state.owner.as_deref() == Some(owner),
            control_error: state.error.clone(),
            human_idle_secs: 60,
        }
    }
    /// Called only after the server validates a complete input message.
    pub fn input(&self, owner: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        Self::expire(&mut state);
        if state.mode != Mode::Human || state.owner.as_deref() != Some(owner) {
            return false;
        }
        state.deadline = Instant::now() + Duration::from_secs(60);
        true
    }
    pub async fn take(&self, owner: &str) -> Result<(), String> {
        let epoch = {
            let mut state = self.state.lock().unwrap();
            Self::expire(&mut state);
            if state.mode == Mode::Human {
                return if state.owner.as_deref() == Some(owner) {
                    Ok(())
                } else {
                    Err("CONTROL_OWNED".into())
                };
            }
            if state.mode == Mode::Blocked {
                return Err("CLEANUP_UNCONFIRMED".into());
            }
            if state.mode == Mode::Transition {
                return Err("CONTROL_TRANSITION".into());
            }
            state.mode = Mode::Transition;
            state.epoch += 1;
            state.error = None;
            if let Some(active) = &state.active {
                active.cancel.cancel();
            }
            state.epoch
        };
        let _handoff = Handoff { gate: self, epoch };
        let done = async {
            loop {
                let notified = self.changed.notified();
                if self.state.lock().unwrap().active.is_none() {
                    return;
                }
                notified.await;
            }
        };
        let ready = tokio::time::timeout(Duration::from_secs(8), done)
            .await
            .is_ok();
        let mut state = self.state.lock().unwrap();
        if state.epoch != epoch {
            return Err("CONTROL_CHANGED".into());
        }
        if !ready || state.error.is_some() {
            state.mode = Mode::Blocked;
            state.error.get_or_insert("CLEANUP_UNCONFIRMED".into());
            return Err(state.error.clone().unwrap());
        }
        state.mode = Mode::Human;
        state.owner = Some(owner.to_owned());
        state.deadline = Instant::now() + Duration::from_secs(60);
        Ok(())
    }
    pub fn give(&self, owner: &str) -> Result<(), &'static str> {
        let mut state = self.state.lock().unwrap();
        Self::expire(&mut state);
        if state.mode == Mode::Ai {
            return Ok(());
        }
        if state.mode != Mode::Human {
            return Err("CONTROL_BLOCKED");
        }
        if state.owner.as_deref() != Some(owner) {
            return Err("CONTROL_OWNED");
        }
        state.mode = Mode::Ai;
        state.epoch += 1;
        state.owner = None;
        Ok(())
    }
    pub fn disconnect(&self, owner: &str) {
        let mut state = self.state.lock().unwrap();
        if state.owner.as_deref() == Some(owner) && state.mode == Mode::Human {
            state.mode = Mode::Ai;
            state.epoch += 1;
            state.owner = None;
        }
    }
}
impl Operation {
    pub fn check(&self) -> Result<(), &'static str> {
        let state = self.gate.state.lock().unwrap();
        if self.cancel.is_cancelled() || state.mode != Mode::Ai || state.epoch != self.epoch {
            Err("CONTROL_CHANGED")
        } else {
            Ok(())
        }
    }
    pub fn cleanup_failed(&self) {
        self.gate.block();
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.id == self.id)
        {
            state.active = None;
        }
        self.gate.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn take_cancels_and_waits_for_owner() {
        let gate = Arc::new(ControlGate::default());
        let operation = gate.begin().unwrap();
        assert_eq!(gate.begin().err(), Some("BUSY"));
        let taking = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.take("a").await })
        };
        operation.cancel.cancelled().await;
        assert!(operation.check().is_err());
        assert_eq!(gate.view("a").control, Mode::Transition);
        drop(operation);
        taking.await.unwrap().unwrap();
        assert!(gate.input("a"));
        assert!(!gate.input("b"));
        assert!(gate.give("b").is_err());
        gate.give("a").unwrap();
        assert!(gate.begin().is_ok());
    }
    #[tokio::test]
    async fn abandoned_handoff_releases_transition_without_releasing_the_operation() {
        let gate = Arc::new(ControlGate::default());
        let operation = gate.begin().unwrap();
        let taking = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.take("viewer").await })
        };
        operation.cancel.cancelled().await;
        taking.abort();
        assert!(taking.await.unwrap_err().is_cancelled());
        assert_eq!(gate.view("viewer").control, Mode::Ai);
        assert_eq!(gate.begin().err(), Some("BUSY"));
        operation.cleanup_failed();
        drop(operation);
        assert_eq!(gate.view("viewer").control, Mode::Blocked);
        assert_eq!(gate.begin().err(), Some("CLEANUP_UNCONFIRMED"));
    }
    #[tokio::test]
    async fn unconfirmed_cleanup_never_grants_human_or_another_operation() {
        let gate = Arc::new(ControlGate::default());
        let operation = gate.begin().unwrap();
        let taking = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.take("viewer").await })
        };
        operation.cancel.cancelled().await;
        operation.cleanup_failed();
        drop(operation);
        assert!(taking.await.unwrap().is_err());
        assert_eq!(gate.view("viewer").control, Mode::Blocked);
        assert!(!gate.input("viewer"));
        assert_eq!(gate.begin().err(), Some("CLEANUP_UNCONFIRMED"));
        assert!(gate.take("viewer").await.is_err());
    }
    #[tokio::test]
    async fn another_viewer_cannot_steal_or_release_control() {
        let gate = Arc::new(ControlGate::default());
        gate.take("owner").await.unwrap();
        assert_eq!(gate.take("other").await.unwrap_err(), "CONTROL_OWNED");
        gate.disconnect("other");
        assert!(gate.view("owner").can_input);
        assert_eq!(gate.begin().err(), Some("HUMAN_CONTROL"));
        gate.disconnect("owner");
        assert_eq!(gate.view("other").control, Mode::Ai);
        assert!(gate.begin().is_ok());
    }
    #[test]
    fn passive_view_does_not_extend_lease() {
        let gate = ControlGate::default();
        {
            let mut state = gate.state.lock().unwrap();
            state.mode = Mode::Human;
            state.owner = Some("a".into());
            state.deadline = Instant::now() - Duration::from_secs(1);
        }
        assert_eq!(gate.view("a").control, Mode::Ai);
        assert!(!gate.input("a"));
    }
}
