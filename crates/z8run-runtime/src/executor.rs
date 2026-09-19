//! WASM node execution layer.
//!
//! Wraps a WASM module as a NodeExecutor for the flow engine. Calls into the
//! module run on the blocking thread pool, so a plugin that computes for a
//! while never stalls the async runtime, and they are bounded by the
//! sandbox's CPU, time and memory limits (A-10).

use crate::manifest::PluginManifest;
use crate::sandbox::{SandboxConfig, WasmInstance, WasmSandbox};
use crate::RuntimeError;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Semaphore;
use tracing::{debug, warn};
use wasmtime::Module;
use z8run_core::engine::{NodeExecutor, NodeExecutorFactory};
use z8run_core::error::{Z8Error, Z8Result};
use z8run_core::message::FlowMessage;

/// Plugin calls allowed to run at once across the whole process (R-06).
/// Per-call limits bound each call; this bounds how many threads and how
/// much memory all of them can take together. `Z8_PLUGIN_MAX_CONCURRENCY`,
/// default: the number of CPU cores.
static PROCESS_SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    let default = std::thread::available_parallelism().map_or(4, |n| n.get());
    let slots = std::env::var("Z8_PLUGIN_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default);
    Arc::new(Semaphore::new(slots))
});

/// A compiled plugin and the limits its instances run under.
struct Plugin {
    sandbox: WasmSandbox,
    module: Module,
    /// Shared call slots (the process-wide pool unless overridden).
    slots: Arc<Semaphore>,
}

/// One node's instance, recreated after a failed call.
struct NodeState {
    plugin: Arc<Plugin>,
    /// `None` after a call failed mid-way: its memory may be inconsistent.
    instance: Option<WasmInstance>,
    /// Last configuration, re-applied to a fresh instance.
    config_json: Option<String>,
}

impl NodeState {
    /// Runs `call` on the instance, creating one if needed. A failed call
    /// discards the instance so the next call starts clean.
    fn with_instance<T>(
        &mut self,
        call: impl FnOnce(&mut WasmInstance) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        if self.instance.is_none() {
            let mut fresh = self
                .plugin
                .sandbox
                .instantiate_module(&self.plugin.module)?;
            if let Some(config) = &self.config_json {
                fresh.call_configure(config)?;
            }
            self.instance = Some(fresh);
        }
        let result = call(self.instance.as_mut().expect("instance was just created"));
        if result.is_err() {
            self.instance = None;
        }
        result
    }
}

/// Runs `call` against the node's instance on the blocking thread pool,
/// once a process-wide call slot is free. Waiting longer than the plugin's
/// own time limit fails instead of queueing without bound.
async fn run_blocking<T: Send + 'static>(
    state: &Arc<Mutex<NodeState>>,
    call: impl FnOnce(&mut NodeState) -> Result<T, RuntimeError> + Send + 'static,
) -> Z8Result<T> {
    let (slots, wait) = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        (
            Arc::clone(&guard.plugin.slots),
            guard.plugin.sandbox.config().timeout,
        )
    };
    let permit = match tokio::time::timeout(wait, slots.acquire_owned()).await {
        Ok(Ok(permit)) => permit,
        _ => {
            let e = RuntimeError::LimitExceeded(
                "all plugin call slots are busy (Z8_PLUGIN_MAX_CONCURRENCY)".to_string(),
            );
            warn!(error = %e, "WASM plugin call rejected");
            return Err(Z8Error::Internal(e.to_string()));
        }
    };
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        // Held until the call really ends, even if the caller stopped
        // waiting: the thread and memory are in use until then.
        let _permit = permit;
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        call(&mut guard)
    })
    .await
    .map_err(|e| Z8Error::Internal(format!("WASM task failed: {}", e)))?
    .map_err(|e| {
        if matches!(e, RuntimeError::LimitExceeded(_)) {
            warn!(error = %e, "WASM plugin stopped by the sandbox");
        }
        Z8Error::Internal(e.to_string())
    })
}

/// Wraps a WASM instance as a NodeExecutor for the flow engine.
pub struct WasmNodeExecutor {
    state: Arc<Mutex<NodeState>>,
    node_type_name: String,
}

#[async_trait::async_trait]
impl NodeExecutor for WasmNodeExecutor {
    async fn process(&self, msg: FlowMessage) -> Z8Result<Vec<FlowMessage>> {
        let payload_json = serde_json::to_string(&msg.payload).map_err(Z8Error::Serialization)?;

        debug!(
            payload_len = payload_json.len(),
            node_type = %self.node_type_name,
            "Processing message in WASM node"
        );

        let result_json = run_blocking(&self.state, move |state| {
            state.with_instance(|instance| instance.call_process(&payload_json))
        })
        .await
        .map_err(|e| Z8Error::Internal(format!("WASM process failed: {}", e)))?;

        // Parse the response as JSON
        let response: serde_json::Value = serde_json::from_str(&result_json)
            .map_err(|e| Z8Error::Internal(format!("Failed to parse WASM response: {}", e)))?;

        // Expect an array of output messages: [{port: string, payload: Value}, ...]
        let outputs = response.as_array().ok_or_else(|| {
            Z8Error::Internal("WASM process must return a JSON array".to_string())
        })?;

        let mut messages = Vec::new();
        for output in outputs {
            let port = output
                .get("port")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Z8Error::Internal("Missing or invalid 'port' field in output".to_string())
                })?
                .to_string();

            let payload = output.get("payload").cloned().ok_or_else(|| {
                Z8Error::Internal("Missing 'payload' field in output".to_string())
            })?;

            messages.push(msg.derive(msg.source_node, port, payload));
        }

        debug!(output_count = messages.len(), "WASM process completed");
        Ok(messages)
    }

    async fn configure(&mut self, config: serde_json::Value) -> Z8Result<()> {
        let config_json = serde_json::to_string(&config).map_err(Z8Error::Serialization)?;
        debug!(config_len = config_json.len(), "Configuring WASM node");

        run_blocking(&self.state, move |state| {
            state.with_instance(|instance| instance.call_configure(&config_json))?;
            state.config_json = Some(config_json);
            Ok(())
        })
        .await
        .map_err(|e| Z8Error::Internal(format!("WASM configure failed: {}", e)))
    }

    async fn validate(&self) -> Z8Result<()> {
        debug!("Validating WASM node configuration");
        run_blocking(&self.state, |state| {
            state.with_instance(|instance| instance.call_validate())
        })
        .await
        .map_err(|e| Z8Error::Internal(format!("WASM validate failed: {}", e)))
    }

    fn node_type(&self) -> &str {
        &self.node_type_name
    }
}

/// Factory that creates WasmNodeExecutor instances from a WASM module.
pub struct WasmNodeFactory {
    plugin: Arc<Plugin>,
    manifest: PluginManifest,
}

impl WasmNodeFactory {
    /// Creates a factory, compiling the module once. Invalid modules are
    /// rejected here, when the plugin is registered.
    pub fn new(
        wasm_bytes: Vec<u8>,
        sandbox_config: SandboxConfig,
        manifest: PluginManifest,
    ) -> Result<Self, RuntimeError> {
        let sandbox = WasmSandbox::new(sandbox_config)?;
        let module = sandbox.compile(&wasm_bytes)?;
        Ok(Self {
            plugin: Arc::new(Plugin {
                sandbox,
                module,
                slots: Arc::clone(&PROCESS_SLOTS),
            }),
            manifest,
        })
    }

    /// Draws call slots from `slots` instead of the process-wide pool, e.g.
    /// to give some plugins their own budget. Factories sharing the same
    /// semaphore share the budget.
    pub fn with_slots(mut self, slots: Arc<Semaphore>) -> Self {
        let plugin = Arc::get_mut(&mut self.plugin)
            .expect("called before any node was created from this factory");
        plugin.slots = slots;
        self
    }

    /// Creates a factory with the operator's limits, narrowed by the
    /// manifest (which can lower the memory limit but not raise it).
    pub fn from_manifest_and_bytes(
        manifest: PluginManifest,
        wasm_bytes: Vec<u8>,
    ) -> Result<Self, RuntimeError> {
        let config = SandboxConfig::for_plugin(&manifest.capabilities);
        Self::new(wasm_bytes, config, manifest)
    }

    /// The limits instances of this plugin run under.
    pub fn sandbox_config(&self) -> &SandboxConfig {
        self.plugin.sandbox.config()
    }
}

#[async_trait::async_trait]
impl NodeExecutorFactory for WasmNodeFactory {
    async fn create(&self, config: serde_json::Value) -> Z8Result<Box<dyn NodeExecutor>> {
        debug!(
            node_type = %self.manifest.name,
            "Creating WASM node executor instance"
        );

        let mut executor = WasmNodeExecutor {
            state: Arc::new(Mutex::new(NodeState {
                plugin: Arc::clone(&self.plugin),
                instance: None,
                config_json: None,
            })),
            node_type_name: self.manifest.name.clone(),
        };

        if !config.is_null() && config != serde_json::Value::Object(Default::default()) {
            executor.configure(config).await?;
            executor.validate().await?;
        } else {
            // Instantiate now so a broken plugin fails at deploy, not on the
            // first message.
            run_blocking(&executor.state, |state| state.with_instance(|_| Ok(())))
                .await
                .map_err(|e| Z8Error::Internal(format!("Failed to instantiate module: {}", e)))?;
        }

        Ok(Box::new(executor))
    }

    fn node_type(&self) -> &str {
        &self.manifest.name
    }
}
