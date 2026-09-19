//! Plugins registered at startup cannot take over built-in node types.

use std::path::Path;

use z8run_core::engine::FlowEngine;
use z8run_runtime::{register_plugins, PluginRegistry};

/// Minimal valid plugin (WAT text; wasmtime compiles it directly).
const WASM: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "z8_alloc") (param i32) (result i32) (i32.const 1024))
  (func (export "z8_process") (param i32 i32) (result i32) (i32.const 0)))"#;

fn install(plugins_dir: &Path, name: &str) {
    let dir = plugins_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.wasm"), WASM).unwrap();
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            r#"name = "{name}"
version = "0.1.0"
description = "test"
author = "test"
category = "transform"
inputs = []
outputs = []
wasm_file = "plugin.wasm"
"#
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn a_plugin_cannot_replace_a_built_in_node() {
    let plugins = std::env::temp_dir().join(format!("z8-plugins-{}", uuid::Uuid::now_v7()));
    install(&plugins, "http-request"); // collides with a built-in
    install(&plugins, "my-transform");

    let engine = FlowEngine::new();
    z8run_core::nodes::register_builtin_nodes(&engine).await;
    let registry = PluginRegistry::new(&plugins);
    assert_eq!(registry.scan().await.unwrap(), 2);

    let registered = register_plugins(&engine, &registry).await.unwrap();
    assert_eq!(registered, 1, "only the non-colliding plugin is registered");
    assert!(engine.has_node_type("my-transform").await);
    assert!(engine.has_node_type("http-request").await);

    let _ = std::fs::remove_dir_all(&plugins);
}
