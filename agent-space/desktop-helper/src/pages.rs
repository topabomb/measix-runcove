//! One Page admission owner for the shared Chromium, including native new windows.
use anyhow::{anyhow, Context};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Request {
    action: Action,
    reply: oneshot::Sender<Result<Option<String>, String>>,
}
#[derive(Clone, Copy)]
enum Action {
    Create,
    Ensure,
    Sync,
    Existing,
}

#[derive(Clone)]
pub struct Pages(Arc<Owner>);
struct Owner {
    requests: mpsc::Sender<Request>,
    violations: watch::Receiver<u64>,
    stop: CancellationToken,
    max: usize,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
impl Pages {
    pub async fn start(max: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(max > 0, "browser.max_pages must be greater than zero");
        let mut policy = Policy {
            max,
            pages: HashMap::new(),
            pending_restore: false,
        };
        let (violations, changed) = watch::channel(0u64);
        let mut cdp = Connection::open(&mut policy, true, &violations).await?;
        let (requests, mut incoming) = mpsc::channel::<Request>(16);
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        tokio::spawn(async move {
            loop {
                let result = tokio::select! {
                    _ = cancelled.cancelled() => break,
                    result = cdp.serve(&mut policy, &mut incoming, &violations) => result,
                };
                if let Err(error) = result {
                    eprintln!("Page guard disconnected: {error}");
                }
                // Retain accepted target identities across a CDP reconnect. Unknown
                // targets must pass admission; a reconnect does not reset the quota.
                loop {
                    tokio::select! {
                        _ = cancelled.cancelled() => return,
                        request = incoming.recv() => match request {
                            Some(request) => {
                                let result = if matches!(request.action, Action::Existing) {
                                    Ok(policy.pages.keys().next().cloned())
                                } else {
                                    Err("BROWSER_UNAVAILABLE: page admission is reconnecting".into())
                                };
                                let _ = request.reply.send(result);
                            },
                            None => return,
                        },
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    }
                    let connected = tokio::select! {
                        _ = cancelled.cancelled() => return,
                        result = Connection::open(&mut policy, false, &violations) => result,
                    };
                    if let Ok(next) = connected {
                        cdp = next;
                        break;
                    }
                }
            }
        });
        Ok(Self(Arc::new(Owner {
            requests,
            violations: changed,
            stop,
            max,
        })))
    }
    pub async fn create(&self) -> anyhow::Result<String> {
        self.request(Action::Create)
            .await?
            .context("missing new page identity")
    }
    pub async fn ensure_page(&self) -> anyhow::Result<Option<String>> {
        self.request(Action::Ensure).await
    }
    pub async fn sync(&self) -> anyhow::Result<()> {
        self.request(Action::Sync).await.map(|_| ())
    }
    pub async fn has_pages(&self) -> anyhow::Result<bool> {
        self.request(Action::Existing).await.map(|id| id.is_some())
    }
    async fn request(&self, action: Action) -> anyhow::Result<Option<String>> {
        let (reply, result) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(5), async {
            self.0
                .requests
                .send(Request { action, reply })
                .await
                .map_err(|_| anyhow!("BROWSER_UNAVAILABLE: page admission stopped"))?;
            result
                .await
                .map_err(|_| anyhow!("BROWSER_UNAVAILABLE: page admission stopped"))?
                .map_err(anyhow::Error::msg)
        })
        .await
        .context("BROWSER_UNAVAILABLE: page admission timed out")?
    }
    pub fn violations(&self) -> watch::Receiver<u64> {
        self.0.violations.clone()
    }
    pub fn limit_error(&self) -> String {
        limit_error(self.0.max)
    }
}
fn limit_error(max: usize) -> String {
    format!("PAGE_LIMIT_REACHED: Page limit reached ({max}). Reuse an existing page or close unused pages before opening another.")
}

struct Policy {
    max: usize,
    // A fresh startup can defer its original session until a profile is chosen.
    pending_restore: bool,
    // Rejected targets remain counted until targetDestroyed confirms closure.
    pages: HashMap<String, bool>,
}
impl Policy {
    fn add(&mut self, id: &str) -> bool {
        if let Some(accepted) = self.pages.get(id) {
            return *accepted;
        }
        let accepted = self.pages.len() < self.max;
        self.pages.insert(id.to_owned(), accepted);
        accepted
    }
    fn snapshot(&mut self, targets: &[Value], baseline: bool) -> Vec<String> {
        let ids: Vec<_> = targets
            .iter()
            .filter(|t| t["type"] == "page")
            .filter_map(|t| t["targetId"].as_str())
            .collect();
        self.pages.retain(|id, _| ids.contains(&id.as_str()));
        let mut rejected = vec![];
        for id in ids {
            if baseline {
                self.pages.insert(id.into(), true);
            } else if !self.add(id) {
                rejected.push(id.into());
            }
        }
        rejected
    }
}
struct Connection {
    socket: Socket,
    sequence: u64,
    responses: VecDeque<Value>,
}
impl Connection {
    async fn open(
        policy: &mut Policy,
        baseline: bool,
        violations: &watch::Sender<u64>,
    ) -> anyhow::Result<Self> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let endpoint = crate::desktop::node(json!({"action":"page_endpoint"})).await?;
            let (socket, _) = connect_async(
                endpoint["websocket"]
                    .as_str()
                    .context("missing browser endpoint")?,
            )
            .await?;
            let mut cdp = Self {
                socket,
                sequence: 0,
                responses: VecDeque::new(),
            };
            cdp.send(
                "Target.setDiscoverTargets",
                json!({"discover":true,"filter":[{"type":"page"}]}),
            )
            .await?;
            let request = cdp
                .send("Target.getTargets", json!({"filter":[{}]}))
                .await?;
            let targets = loop {
                let value = cdp.receive().await?;
                if value["id"] == request {
                    break value["result"]["targetInfos"]
                        .as_array()
                        .context("missing browser targets")?
                        .clone();
                }
            };
            let restoring =
                baseline || (policy.pending_restore && targets.iter().any(|t| t["type"] == "page"));
            let previous = policy.pages.clone();
            let rejected = policy.snapshot(&targets, restoring);
            if restoring && !policy.pages.is_empty() {
                policy.pending_restore = false;
            }
            if baseline {
                policy.pending_restore = policy.pages.is_empty()
                    && targets.iter().any(|t| {
                        t["url"]
                            .as_str()
                            .is_some_and(|url| url.starts_with("chrome://profile-picker/"))
                    });
            }
            for id in rejected {
                if !previous.contains_key(&id) {
                    violations.send_modify(|count| *count = count.wrapping_add(1));
                }
                cdp.close(&id).await?;
            }
            if restoring {
                cdp.restore_burst(policy).await?;
            }
            Ok(cdp)
        })
        .await
        .context("browser page guard startup timed out")?
    }
    async fn restore_burst(&mut self, policy: &mut Policy) -> anyhow::Result<()> {
        // Session restore creates targets before their documents load.
        // The picker can defer this same initial burst until selection.
        let until = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut responses = VecDeque::new();
        while tokio::time::Instant::now() < until {
            match tokio::time::timeout(Duration::from_millis(200), self.receive_raw()).await {
                Ok(Ok(value)) => {
                    let info = &value["params"]["targetInfo"];
                    if value["method"] == "Target.targetCreated" && info["type"] == "page" {
                        if let Some(id) = info["targetId"].as_str() {
                            policy.pages.insert(id.into(), true);
                            policy.pending_restore = false;
                        }
                    } else if value["method"] == "Target.targetDestroyed" {
                        if let Some(id) = value["params"]["targetId"].as_str() {
                            policy.pages.remove(id);
                        }
                    } else if value.get("id").is_some() {
                        responses.push_back(value);
                    }
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => break,
            }
        }
        self.responses.extend(responses);
        Ok(())
    }
    async fn send(&mut self, method: &str, params: Value) -> anyhow::Result<u64> {
        self.sequence += 1;
        self.socket
            .send(Message::Text(
                json!({"id":self.sequence,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await?;
        Ok(self.sequence)
    }
    async fn receive(&mut self) -> anyhow::Result<Value> {
        if let Some(response) = self.responses.pop_front() {
            return Ok(response);
        }
        self.receive_raw().await
    }
    async fn receive_raw(&mut self) -> anyhow::Result<Value> {
        loop {
            match self.socket.next().await.context("browser disconnected")?? {
                Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                Message::Close(_) => return Err(anyhow!("browser disconnected")),
                _ => {}
            }
        }
    }
    async fn close(&mut self, id: &str) -> anyhow::Result<()> {
        self.send("Target.closeTarget", json!({"targetId":id}))
            .await?;
        Ok(())
    }
    async fn event(
        &mut self,
        value: &Value,
        policy: &mut Policy,
        violations: &watch::Sender<u64>,
    ) -> anyhow::Result<()> {
        match value["method"].as_str() {
            Some("Target.targetCreated") if value["params"]["targetInfo"]["type"] == "page" => {
                if let Some(id) = value["params"]["targetInfo"]["targetId"].as_str() {
                    if policy.pending_restore {
                        policy.pending_restore = false;
                        policy.pages.insert(id.into(), true);
                        return self.restore_burst(policy).await;
                    }
                    let known = policy.pages.contains_key(id);
                    if !policy.add(id) {
                        if !known {
                            violations.send_modify(|count| *count = count.wrapping_add(1));
                            eprintln!("{}", limit_error(policy.max));
                        }
                        self.close(id).await?;
                    }
                }
            }
            Some("Target.targetDestroyed") => {
                if let Some(id) = value["params"]["targetId"].as_str() {
                    policy.pages.remove(id);
                }
            }
            _ => {}
        }
        Ok(())
    }
    async fn create(
        &mut self,
        policy: &mut Policy,
        violations: &watch::Sender<u64>,
        action: Action,
    ) -> anyhow::Result<Option<String>> {
        // A page.close() reply can precede targetDestroyed on this separate CDP
        // connection. Refresh before admission so a confirmed close is usable.
        let mut scan = self
            .send("Target.getTargets", json!({"filter":[{}]}))
            .await?;
        let profile_picker = loop {
            let value = self.receive().await?;
            let pending_restore = policy.pending_restore;
            self.event(&value, policy, violations).await?;
            if pending_restore && !policy.pending_restore {
                // Restore drained events beyond the original snapshot reply.
                // Refresh so that an older reply cannot discard restored IDs.
                scan = self
                    .send("Target.getTargets", json!({"filter":[{}]}))
                    .await?;
                continue;
            }
            if value["id"] == scan {
                let targets = value["result"]["targetInfos"]
                    .as_array()
                    .context("missing browser targets")?;
                let profile_picker = targets.iter().any(|t| {
                    t["url"]
                        .as_str()
                        .is_some_and(|url| url.starts_with("chrome://profile-picker/"))
                });
                let previous = policy.pages.clone();
                for id in policy.snapshot(targets, false) {
                    if !previous.contains_key(&id) {
                        violations.send_modify(|count| *count = count.wrapping_add(1));
                    }
                    self.close(&id).await?;
                }
                break profile_picker;
            }
        };
        if matches!(action, Action::Sync) {
            return Ok(None);
        }
        let only_if_empty = matches!(action, Action::Ensure);
        if only_if_empty {
            if let Some((id, _)) = policy.pages.iter().find(|(_, accepted)| **accepted) {
                return Ok(Some(id.clone()));
            }
        }
        // The picker is browser UI, not a Page. Leave profile selection to the
        // user/desktop controls rather than opening a tab in an arbitrary profile.
        if policy.pages.is_empty() && profile_picker {
            if only_if_empty {
                return Ok(None);
            }
            anyhow::bail!("PROFILE_SELECTION_REQUIRED: Select a Chrome profile using the desktop, then observe again.");
        }
        anyhow::ensure!(
            policy.pages.len() < policy.max,
            "{}",
            limit_error(policy.max)
        );
        let request = self
            .send("Target.createTarget", json!({"url":"about:blank"}))
            .await?;
        loop {
            let value = self.receive().await?;
            self.event(&value, policy, violations).await?;
            if value["id"] == request {
                anyhow::ensure!(
                    value.get("error").is_none(),
                    "BROWSER_UNAVAILABLE: {}",
                    value["error"]
                );
                let id = value["result"]["targetId"]
                    .as_str()
                    .context("missing new page identity")?;
                if !policy.add(id) {
                    self.close(id).await?;
                    anyhow::bail!(limit_error(policy.max));
                }
                return Ok(Some(id.into()));
            }
        }
    }
    async fn serve(
        &mut self,
        policy: &mut Policy,
        incoming: &mut mpsc::Receiver<Request>,
        violations: &watch::Sender<u64>,
    ) -> anyhow::Result<()> {
        let mut retry = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                request = incoming.recv() => {
                    let request = request.context("page owner stopped")?;
                    let reply = request.reply;
                    if reply.is_closed() { continue; }
                    if matches!(request.action, Action::Existing) {
                        let _ = reply.send(Ok(policy.pages.keys().next().cloned()));
                        continue;
                    }
                    let result = tokio::time::timeout(Duration::from_secs(3), self.create(policy, violations, request.action)).await;
                    let timed_out = result.is_err();
                    let result = result.unwrap_or_else(|_| Err(anyhow!("BROWSER_UNAVAILABLE: page creation timed out")));
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                    if timed_out { anyhow::bail!("page creation timed out; resynchronizing targets"); }
                },
                value = self.receive() => self.event(&value?, policy, violations).await?,
                _ = retry.tick() => {
                    for id in policy.pages.iter().filter_map(|(id, accepted)| (!accepted).then_some(id)) { self.close(id).await?; }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_counts_rejected_pages_until_confirmed_closed() {
        let mut p = Policy {
            max: 1,
            pending_restore: false,
            pages: HashMap::new(),
        };
        assert!(p.add("existing"));
        assert!(!p.add("overflow"));
        p.pages.remove("existing");
        assert!(!p.add("next"));
        p.pages.remove("overflow");
        p.pages.remove("next");
        assert!(p.add("next"));
    }
    #[test]
    fn baseline_preserves_existing_pages_but_reconnect_does_not_adopt_new_overflow() {
        let mut p = Policy {
            max: 1,
            pending_restore: false,
            pages: HashMap::new(),
        };
        let targets = vec![
            json!({"type":"page","targetId":"a"}),
            json!({"type":"page","targetId":"b"}),
            json!({"type":"iframe","targetId":"frame"}),
        ];
        assert!(p.snapshot(&targets, true).is_empty());
        assert_eq!(p.pages.len(), 2);
        assert!(!p.add("c"));
        assert_eq!(
            p.snapshot(
                &[
                    json!({"type":"page","targetId":"a"}),
                    json!({"type":"page","targetId":"new"})
                ],
                false
            ),
            vec!["new"]
        );
        assert!(p.pages["a"]);
        assert!(!p.pages["new"]);
    }
}
