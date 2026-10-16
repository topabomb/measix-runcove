//! Fixed helper ownership, shared by VM and local-process transports.
use super::{EngineError, ManagedInstance, ManagedLaunch, ManagedService};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[async_trait]
pub(super) trait ManagedRuntime: Send + Sync {
    async fn action(&self, action: &str, payload: &Value) -> Result<Value, EngineError>;
    async fn copy_binary(&self, source: &str, destination: &str) -> Result<(), EngineError>;
}

pub(super) async fn request<T: DeserializeOwned>(
    runtime: &impl ManagedRuntime,
    action: &str,
    payload: &Value,
) -> Result<T, EngineError> {
    serde_json::from_value(runtime.action(action, payload).await?)
        .map_err(|e| EngineError::Other(format!("managed {action} returned invalid identity: {e}")))
}

pub(super) async fn start(
    runtime: &impl ManagedRuntime,
    launch: &ManagedLaunch,
) -> Result<ManagedInstance, EngineError> {
    let binary = tokio::fs::read(launch.host_binary())
        .await
        .map_err(|e| EngineError::Other(format!("read helper: {e}")))?;
    let binary_digest = hex::encode(Sha256::digest(&binary));
    drop(binary);
    let settings = launch.settings();
    let launch_digest = hex::encode(Sha256::digest(serde_json::to_vec(&settings).unwrap()));
    let service = launch.service();
    let mut reusable = None;
    let existing: Vec<ManagedInstance> =
        request(runtime, "list", &json!({"service":service})).await?;
    for instance in existing {
        let alive: bool = request(runtime, "alive", &json!(instance)).await?;
        if matches!(service, ManagedService::Webdav | ManagedService::Desktop)
            && alive
            && instance.port != 0
            && instance.binary_digest == binary_digest
            && instance.launch_digest == launch_digest
            && reusable.is_none()
        {
            reusable = Some(instance);
        } else if !alive {
            runtime.action("stop", &json!(instance)).await?;
        } else if matches!(service, ManagedService::Webdav | ManagedService::Desktop) {
            return Err(EngineError::ManagedRestartRequired(Box::new(instance)));
        }
    }
    if let Some(instance) = reusable {
        runtime
            .action(
                "project",
                &json!({"instance":instance,"settings":settings,"readme":include_str!("../../guest/README.md")}),
            )
            .await?;
        tracing::info!(service=?service,instance=%instance.instance_id,port=instance.port,"adopted managed service");
        return Ok(instance);
    }
    let instance = ManagedInstance {
        service,
        instance_id: uuid::Uuid::new_v4().simple().to_string(),
        pid: 0,
        start_ticks: 0,
        boot_id: String::new(),
        executable_device: 0,
        executable_inode: 0,
        binary_digest,
        launch_digest,
        port: 0,
    };
    let payload = json!({"instance":instance,"settings":settings,"readme":include_str!("../../guest/README.md")});
    runtime.action("prepare", &payload).await?;
    let temporary = format!("{}.{}.tmp", service.executable(), instance.instance_id);
    let copied = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        runtime.copy_binary(launch.host_binary(), &temporary),
    )
    .await
    .map_err(|_| {
        EngineError::Timeout("managed helper copy exceeded 15s; ownership retained".into())
    })?;
    if let Err(error) = copied {
        let _ = runtime.action("stop", &json!(instance)).await;
        return Err(error);
    }
    let result: Result<ManagedInstance, EngineError> = request(runtime, "start", &payload).await;
    match &result {
        Ok(instance) => {
            tracing::info!(service=?service,instance=%instance.instance_id,port=instance.port,"started managed service")
        }
        Err(error) => {
            tracing::warn!(service=?service,instance=%instance.instance_id,%error,"managed startup failed; ownership retained if cleanup is incomplete")
        }
    }
    result
}
