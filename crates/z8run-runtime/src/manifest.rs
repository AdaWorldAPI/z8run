//! WASM plugin manifest.
//!
//! Each plugin is distributed with a manifest that declares
//! metadata, ports, required host capabilities, etc.

use serde::{Deserialize, Serialize};

/// WASM plugin/node manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin name (e.g., "http-request").
    pub name: String,
    /// Semantic version of the plugin.
    pub version: String,
    /// Human-readable description.
    pub description: String,
    /// Plugin author.
    pub author: String,
    /// License.
    #[serde(default)]
    pub license: String,
    /// Category for the editor (e.g., "network", "transform", "io").
    pub category: String,
    /// Node icon in the editor (name or URL).
    #[serde(default)]
    pub icon: String,
    /// Input port definitions.
    pub inputs: Vec<ManifestPort>,
    /// Output port definitions.
    pub outputs: Vec<ManifestPort>,
    /// Required WASI capabilities.
    #[serde(default)]
    pub capabilities: PluginCapabilities,
    /// WASM file relative to manifest.
    pub wasm_file: String,
    /// Minimum z8run runtime version required.
    #[serde(default)]
    pub min_runtime_version: String,
    /// Default node configuration, shown as editable fields in the editor and
    /// passed to `z8_configure`. Written as a `[config]` table in TOML.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub config: serde_json::Value,
}

/// Port declared in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPort {
    pub name: String,
    #[serde(rename = "type")]
    pub port_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

/// WASI capabilities that the plugin requests from the host.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// Network access (making HTTP requests, etc.).
    #[serde(default)]
    pub network: bool,
    /// Filesystem access (reading/writing files).
    #[serde(default)]
    pub filesystem: bool,
    /// Allowed directories if filesystem = true.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// Environment variable access.
    #[serde(default)]
    pub env_vars: bool,
    /// Specific environment variables allowed.
    #[serde(default)]
    pub allowed_env: Vec<String>,
    /// Memory limit in MB (0 = use system default).
    #[serde(default)]
    pub memory_limit_mb: u64,
}

impl PluginManifest {
    /// Loads a manifest from a TOML file.
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }

    /// Checks the fields that become identifiers and paths on the server.
    ///
    /// The name is the node type and the plugin's directory name, so it is
    /// limited to lowercase letters, digits, `-` and `_` (1-64 chars). The
    /// wasm file must be a relative path inside the plugin directory.
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_plugin_name(&self.name) {
            return Err(format!(
                "invalid plugin name '{}': use 1-64 lowercase letters, digits, '-' or '_', \
                 starting with a letter or digit",
                self.name
            ));
        }
        let wasm = std::path::Path::new(&self.wasm_file);
        let inside = !self.wasm_file.is_empty()
            && wasm
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !inside {
            return Err(format!(
                "wasm_file '{}' must be a relative path inside the plugin directory",
                self.wasm_file
            ));
        }
        if !(self.config.is_null() || self.config.is_object()) {
            return Err("[config] must be a table".to_string());
        }
        Ok(())
    }
}

/// Whether `name` can be used as a plugin name (see [`PluginManifest::validate`]).
pub fn is_valid_plugin_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

impl PluginManifest {
    /// Serializes the manifest to TOML.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}
