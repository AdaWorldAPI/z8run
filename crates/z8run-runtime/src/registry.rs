//! WASM plugin registry.
//!
//! Manages loading, unloading, and discovery of WASM modules
//! available for the flow engine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::manifest::{is_valid_plugin_name, ManifestPort, PluginManifest};
use crate::sandbox::WasmSandbox;
use crate::RuntimeError;

/// Exports a module needs to work as a node.
const REQUIRED_EXPORTS: &[&str] = &["memory", "z8_alloc", "z8_process"];

/// Information about a registered plugin.
#[derive(Debug, Clone)]
pub struct RegisteredPlugin {
    /// Plugin manifest.
    pub manifest: PluginManifest,
    /// Directory the plugin was loaded from.
    pub dir: PathBuf,
    /// Path to the WASM file.
    pub wasm_path: PathBuf,
    /// Whether the module is preloaded in memory.
    pub preloaded: bool,
}

/// Registry of available WASM plugins.
pub struct PluginRegistry {
    /// Plugins registered by name.
    plugins: Arc<RwLock<HashMap<String, RegisteredPlugin>>>,
    /// Base directory where plugins are stored.
    plugins_dir: PathBuf,
}

impl PluginRegistry {
    /// Creates a new registry.
    pub fn new(plugins_dir: impl Into<PathBuf>) -> Self {
        Self {
            plugins: Arc::new(RwLock::new(HashMap::new())),
            plugins_dir: plugins_dir.into(),
        }
    }

    /// Scans the plugins directory and registers found plugins.
    pub async fn scan(&self) -> Result<usize, RuntimeError> {
        let dir = &self.plugins_dir;
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(|e| {
                RuntimeError::ModuleLoad(format!("Could not create plugins directory: {}", e))
            })?;
            return Ok(0);
        }

        let mut count = 0;
        let entries = std::fs::read_dir(dir)
            .map_err(|e| RuntimeError::ModuleLoad(format!("Could not read directory: {}", e)))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Look for manifest.toml in the plugin directory
                let manifest_path = path.join("manifest.toml");
                if manifest_path.exists() {
                    match self.register_from_dir(&path).await {
                        Ok(name) => {
                            tracing::info!(plugin = %name, "Plugin registered");
                            count += 1;
                        }
                        Err(e) => {
                            tracing::warn!(path = %path.display(), error = %e, "Plugin ignored");
                        }
                    }
                }
            }
        }

        Ok(count)
    }

    /// Registers a plugin from its directory.
    async fn register_from_dir(&self, dir: &Path) -> Result<String, RuntimeError> {
        let manifest_content = std::fs::read_to_string(dir.join("manifest.toml"))
            .map_err(|e| RuntimeError::Manifest(e.to_string()))?;

        let manifest = PluginManifest::from_toml(&manifest_content)
            .map_err(|e| RuntimeError::Manifest(e.to_string()))?;
        manifest.validate().map_err(RuntimeError::Manifest)?;

        let wasm_path = dir.join(&manifest.wasm_file);
        if !wasm_path.exists() {
            return Err(RuntimeError::ModuleNotFound(
                wasm_path.display().to_string(),
            ));
        }

        let name = manifest.name.clone();
        self.plugins.write().await.insert(
            name.clone(),
            RegisteredPlugin {
                manifest,
                dir: dir.to_path_buf(),
                wasm_path,
                preloaded: false,
            },
        );

        Ok(name)
    }

    /// Gets a plugin by name.
    pub async fn get(&self, name: &str) -> Option<RegisteredPlugin> {
        self.plugins.read().await.get(name).cloned()
    }

    /// Lists all registered plugins.
    pub async fn list(&self) -> Vec<RegisteredPlugin> {
        self.plugins.read().await.values().cloned().collect()
    }

    /// Returns the count of registered plugins.
    pub async fn count(&self) -> usize {
        self.plugins.read().await.len()
    }

    /// Returns the plugins directory path.
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    /// Installs a plugin from a local .wasm file or a plugin directory.
    ///
    /// A directory must contain `manifest.toml` and the wasm file it names;
    /// only those two files are copied (not build output such as `target/`).
    /// A single `.wasm` file gets a generated manifest named after the file.
    /// The module is compiled and checked for the z8 exports first, and
    /// nothing is left behind if installation fails. Names in `reserved`
    /// (the built-in node types) are refused, since the server would not
    /// load a plugin that shadows one.
    pub async fn install_local(
        &self,
        source: &Path,
        reserved: &[String],
    ) -> Result<String, RuntimeError> {
        if !source.exists() {
            return Err(RuntimeError::ModuleNotFound(source.display().to_string()));
        }

        let (manifest, wasm_bytes) = if source.is_dir() {
            let content = std::fs::read_to_string(source.join("manifest.toml"))
                .map_err(|e| RuntimeError::Manifest(format!("manifest.toml: {}", e)))?;
            let manifest = PluginManifest::from_toml(&content)
                .map_err(|e| RuntimeError::Manifest(e.to_string()))?;
            manifest.validate().map_err(RuntimeError::Manifest)?;
            let wasm_bytes = std::fs::read(source.join(&manifest.wasm_file)).map_err(|e| {
                RuntimeError::ModuleNotFound(format!("{}: {}", manifest.wasm_file, e))
            })?;
            (manifest, wasm_bytes)
        } else if source.extension().is_some_and(|e| e == "wasm") {
            let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let name = stem.to_ascii_lowercase().replace([' ', '.'], "-");
            if !is_valid_plugin_name(&name) {
                return Err(RuntimeError::Manifest(format!(
                    "cannot derive a plugin name from '{stem}'; rename the file \
                     (lowercase letters, digits, '-' or '_')"
                )));
            }
            let wasm_bytes = std::fs::read(source)
                .map_err(|e| RuntimeError::ModuleLoad(format!("Failed to read wasm: {}", e)))?;
            (generated_manifest(&name, source), wasm_bytes)
        } else {
            return Err(RuntimeError::ModuleLoad(
                "Source must be a .wasm file or a directory with manifest.toml".into(),
            ));
        };

        if reserved.contains(&manifest.name) {
            return Err(RuntimeError::Manifest(format!(
                "'{}' is the name of a built-in node; rename the plugin",
                manifest.name
            )));
        }
        check_module(&wasm_bytes)?;

        let dest = self.plugins_dir.join(&manifest.name);
        if dest.exists() {
            return Err(RuntimeError::Manifest(format!(
                "A plugin named '{}' is already installed. Remove it first.",
                manifest.name
            )));
        }

        let written = (|| -> Result<(), RuntimeError> {
            let wasm_dest = dest.join(&manifest.wasm_file);
            if let Some(parent) = wasm_dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    RuntimeError::ModuleLoad(format!("Failed to create plugin dir: {}", e))
                })?;
            }
            std::fs::write(&wasm_dest, &wasm_bytes)
                .map_err(|e| RuntimeError::ModuleLoad(format!("Failed to copy wasm: {}", e)))?;
            let toml = manifest
                .to_toml()
                .map_err(|e| RuntimeError::Manifest(e.to_string()))?;
            std::fs::write(dest.join("manifest.toml"), toml)
                .map_err(|e| RuntimeError::Manifest(format!("Failed to write manifest: {}", e)))
        })();

        match written {
            Ok(()) => self.register_from_dir(&dest).await,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dest);
                Err(e)
            }
        }
    }

    /// Removes an installed plugin by name, deleting the directory it was
    /// loaded from. Call [`Self::scan`] first so installed plugins are known.
    pub async fn remove(&self, name: &str) -> Result<(), RuntimeError> {
        let plugin = self
            .plugins
            .read()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::ModuleNotFound(format!("Plugin '{}' is not installed", name))
            })?;

        // Only ever delete a direct child of the plugins directory.
        let root = self
            .plugins_dir
            .canonicalize()
            .map_err(|e| RuntimeError::ModuleLoad(e.to_string()))?;
        let dir = plugin
            .dir
            .canonicalize()
            .map_err(|e| RuntimeError::ModuleLoad(e.to_string()))?;
        if dir.parent() != Some(root.as_path()) {
            return Err(RuntimeError::ModuleLoad(format!(
                "Refusing to remove {}: not inside {}",
                dir.display(),
                root.display()
            )));
        }

        std::fs::remove_dir_all(&dir).map_err(|e| {
            RuntimeError::ModuleLoad(format!("Failed to remove plugin directory: {}", e))
        })?;
        self.plugins.write().await.remove(name);
        Ok(())
    }
}

/// Manifest for a plugin installed from a bare `.wasm` file: one `input`
/// and one `output` port of any type.
fn generated_manifest(name: &str, source: &Path) -> PluginManifest {
    let port = |port_name: &str| ManifestPort {
        name: port_name.to_string(),
        port_type: "any".to_string(),
        description: String::new(),
        required: false,
    };
    PluginManifest {
        name: name.to_string(),
        version: "0.1.0".to_string(),
        description: format!(
            "Installed from {}",
            source.file_name().unwrap_or_default().to_string_lossy()
        ),
        author: String::new(),
        license: String::new(),
        category: "plugin".to_string(),
        icon: String::new(),
        inputs: vec![port("input")],
        outputs: vec![port("output")],
        capabilities: Default::default(),
        wasm_file: "plugin.wasm".to_string(),
        min_runtime_version: String::new(),
        config: serde_json::Value::Null,
    }
}

/// Compiles the module and checks it has the exports a node needs, so a
/// broken plugin is rejected at install rather than at the next restart.
fn check_module(wasm_bytes: &[u8]) -> Result<(), RuntimeError> {
    let sandbox = WasmSandbox::default_sandbox()?;
    let module = sandbox.compile(wasm_bytes)?;
    let exports: Vec<&str> = module.exports().map(|e| e.name()).collect();
    let missing: Vec<&str> = REQUIRED_EXPORTS
        .iter()
        .copied()
        .filter(|name| !exports.contains(name))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError::ModuleLoad(format!(
            "not a z8run plugin: missing exports {}",
            missing.join(", ")
        )))
    }
}
