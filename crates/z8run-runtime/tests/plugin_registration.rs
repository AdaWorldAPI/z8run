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
    let names: Vec<&str> = registered.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        ["my-transform"],
        "only the non-colliding plugin is registered"
    );
    assert!(engine.has_node_type("my-transform").await);
    assert!(engine.has_node_type("http-request").await);

    let _ = std::fs::remove_dir_all(&plugins);
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("z8-install-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn a_bare_wasm_file_installs_and_loads_after_a_restart() {
    let base = scratch();
    let src = base.join("CSV Parser.wasm");
    std::fs::write(&src, WASM).unwrap();

    let registry = PluginRegistry::new(base.join("plugins"));
    assert_eq!(
        registry.install_local(&src, &[]).await.unwrap(),
        "csv-parser"
    );

    // A new process sees it, with a complete generated manifest.
    let fresh = PluginRegistry::new(base.join("plugins"));
    assert_eq!(fresh.scan().await.unwrap(), 1);
    let plugin = fresh.get("csv-parser").await.unwrap();
    assert_eq!(plugin.manifest.inputs[0].name, "input");
    assert_eq!(plugin.manifest.outputs[0].name, "output");
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn broken_modules_are_rejected_and_leave_nothing_behind() {
    let base = scratch();
    let plugins = base.join("plugins");
    let registry = PluginRegistry::new(&plugins);

    let garbage = base.join("garbage.wasm");
    std::fs::write(&garbage, b"not wasm").unwrap();
    assert!(registry.install_local(&garbage, &[]).await.is_err());

    let no_exports = base.join("empty.wasm");
    std::fs::write(&no_exports, "(module)").unwrap();
    let err = registry.install_local(&no_exports, &[]).await.unwrap_err();
    assert!(err.to_string().contains("missing exports"), "{err}");

    assert!(!plugins.join("garbage").exists() && !plugins.join("empty").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn a_directory_install_copies_only_the_manifest_and_module() {
    let base = scratch();
    let src = base.join("source");
    install(&base, "source"); // writes source/manifest.toml + plugin.wasm
    std::fs::rename(base.join("source"), &src).ok();
    std::fs::create_dir_all(src.join("target/release")).unwrap();
    std::fs::write(src.join("target/release/big.bin"), vec![0u8; 4096]).unwrap();
    std::fs::write(src.join("notes.txt"), "not part of the plugin").unwrap();

    let registry = PluginRegistry::new(base.join("plugins"));
    assert_eq!(registry.install_local(&src, &[]).await.unwrap(), "source");

    let mut files: Vec<String> = std::fs::read_dir(base.join("plugins/source"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["manifest.toml", "plugin.wasm"]);

    // Installing the same name again is refused.
    let again = registry.install_local(&src, &[]).await.unwrap_err();
    assert!(again.to_string().contains("already installed"), "{again}");
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn manifests_cannot_point_outside_their_directory_or_use_odd_names() {
    let base = scratch();
    let plugins = base.join("plugins");
    install(&plugins, "escape");
    let manifest = plugins.join("escape/manifest.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("plugin.wasm", "../../secret.wasm")).unwrap();

    install(&plugins, "Bad Name");

    let registry = PluginRegistry::new(&plugins);
    assert_eq!(registry.scan().await.unwrap(), 0, "both are ignored");
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn remove_works_from_a_fresh_process_and_only_inside_the_plugins_dir() {
    let base = scratch();
    let plugins = base.join("plugins");
    install(&plugins, "to-remove");

    // Like `z8run plugin remove`: a new registry, scanned first.
    let registry = PluginRegistry::new(&plugins);
    registry.scan().await.unwrap();
    registry.remove("to-remove").await.unwrap();
    assert!(!plugins.join("to-remove").exists());

    let missing = registry.remove("../plugins").await.unwrap_err();
    assert!(missing.to_string().contains("not installed"), "{missing}");
    assert!(plugins.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn a_config_table_is_the_default_node_config() {
    let base = scratch();
    let plugins = base.join("plugins");
    install(&plugins, "with-config");
    let manifest = plugins.join("with-config/manifest.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str("\n[config]\nthreshold = 5\nmode = \"strict\"\n");
    std::fs::write(&manifest, text).unwrap();

    let registry = PluginRegistry::new(&plugins);
    registry.scan().await.unwrap();
    let config = registry.get("with-config").await.unwrap().manifest.config;
    assert_eq!(
        config,
        serde_json::json!({ "threshold": 5, "mode": "strict" })
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn built_in_names_are_refused_at_install() {
    let base = scratch();
    let src = base.join("http-request.wasm");
    std::fs::write(&src, WASM).unwrap();
    let registry = PluginRegistry::new(base.join("plugins"));
    let err = registry
        .install_local(&src, &["http-request".to_string()])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("built-in node"), "{err}");
    assert!(!base.join("plugins/http-request").exists());
    let _ = std::fs::remove_dir_all(&base);
}
