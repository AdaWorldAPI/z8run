//! GET /api/v1/plugins lists registered plugins for the editor palette.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use z8run_api::{build_router, state::AppState};
use z8run_runtime::PluginManifest;
use z8run_storage::credential_vault::SqliteCredentialVault;
use z8run_storage::sqlite::SqliteStorage;

async fn test_app() -> (Router, Arc<AppState>) {
    std::env::set_var("Z8_RATE_LIMIT_AUTH", "0");
    std::env::set_var("Z8_RATE_LIMIT_API", "0");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let storage = Arc::new(SqliteStorage::from_pool(pool.clone()));
    storage.migrate().await.unwrap();
    let vault = Arc::new(SqliteCredentialVault::new(pool, "test-vault-secret"));
    let state = Arc::new(AppState::new(
        storage.clone(),
        storage.clone(),
        storage,
        vault,
        "plugins-api-test-jwt-secret".to_string(),
        0,
    ));
    (build_router(Arc::clone(&state)), state)
}

async fn get(app: &Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn token(app: &Router) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/auth/register")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"email": "plugins@example.com", "username": "pluginuser", "password": "password123"})
                .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()["set-cookie"].to_str().unwrap().to_string();
    cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string()
}

#[tokio::test]
async fn plugins_are_listed_for_signed_in_users_in_editor_shape() {
    let (app, state) = test_app().await;
    let manifest: PluginManifest = toml_manifest();
    state.set_plugin_nodes(vec![manifest]);

    let (anonymous, _) = get(&app, "/api/v1/plugins", None).await;
    assert_eq!(anonymous, StatusCode::UNAUTHORIZED);

    let token = token(&app).await;
    let (status, body) = get(&app, "/api/v1/plugins", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["plugins"],
        json!([{
            "type": "row-scorer",
            "label": "row-scorer",
            "description": "Scores rows",
            "version": "1.2.0",
            "author": "someone",
            "icon": "",
            "inputs": [{"id": "rows", "name": "rows", "type": "array", "required": true}],
            // Unknown port types are shown as `any`.
            "outputs": [{"id": "scored", "name": "scored", "type": "any", "required": false}],
            "defaultConfig": {"threshold": 5}
        }])
    );
}

fn toml_manifest() -> PluginManifest {
    PluginManifest::from_toml(
        r#"
name = "row-scorer"
version = "1.2.0"
description = "Scores rows"
author = "someone"
category = "transform"
wasm_file = "plugin.wasm"
inputs = [{ name = "rows", type = "array", required = true }]
outputs = [{ name = "scored", type = "table" }]

[config]
threshold = 5
"#,
    )
    .unwrap()
}
