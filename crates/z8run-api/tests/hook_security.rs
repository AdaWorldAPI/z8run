//! HTTP-level regression tests for the hook and flow-ownership fixes from the
//! 2026-09-18 security audit (A-01, A-02, A-05).
//!
//! The whole scenario runs in ONE test on ONE `AppState`: the `http-out`
//! responder map and the rate limiters are process-wide `OnceLock`s, so only
//! the first state built in a process is wired to them.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use z8run_api::{build_router, state::AppState};
use z8run_storage::credential_vault::SqliteCredentialVault;
use z8run_storage::sqlite::SqliteStorage;

async fn test_app() -> Router {
    // 0 disables a limiter (SEC-006); keep the suite from being throttled.
    for var in [
        "Z8_RATE_LIMIT_API",
        "Z8_RATE_LIMIT_AUTH",
        "Z8_RATE_LIMIT_HOOK",
    ] {
        std::env::set_var(var, "0");
    }

    // A single shared connection keeps the in-memory database alive.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    let storage = Arc::new(SqliteStorage::from_pool(pool.clone()));
    storage.migrate().await.expect("migrate");
    let vault = Arc::new(SqliteCredentialVault::new(pool, "test-vault-secret"));

    let state = Arc::new(AppState::new(
        storage.clone(),
        storage.clone(),
        storage,
        vault,
        "integration-test-jwt-secret".to_string(),
        0,
    ));
    z8run_core::nodes::register_builtin_nodes(&state.engine).await;
    build_router(state)
}

/// Sends a request and returns `(status, json body or Null)`.
async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Registers a user and returns their bearer token.
async fn register(app: &Router, name: &str) -> String {
    let (status, body) = send(
        app,
        "POST",
        "/auth/register",
        None,
        Some(json!({
            "email": format!("{name}@example.com"),
            "username": name,
            "password": "password123",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {body}");
    body["token"].as_str().unwrap().to_string()
}

fn trigger(id: &str, path: &str, auth: Value) -> Value {
    let mut config = json!({"method": "POST", "path": path});
    config
        .as_object_mut()
        .unwrap()
        .extend(auth.as_object().unwrap().clone());
    json!({"id": id, "data": {"type": "webhook-trigger", "label": id, "config": config}})
}

fn http_out(id: &str, status: u16) -> Value {
    json!({"id": id, "data": {"type": "http-out", "label": id, "config": {"statusCode": status}}})
}

fn edge(source: &str, target: &str) -> Value {
    json!({"source": source, "target": target})
}

/// Creates a flow owned by `token` and saves the given canvas. Returns its id.
async fn create_flow(app: &Router, token: &str, nodes: Vec<Value>, edges: Vec<Value>) -> String {
    let (status, body) = send(
        app,
        "POST",
        "/api/v1/flows",
        Some(token),
        Some(json!({"name": "hooks"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create flow: {body}");
    let id = body["id"].as_str().unwrap().to_string();
    save_canvas(app, token, &id, nodes, edges).await;
    id
}

async fn save_canvas(app: &Router, token: &str, id: &str, nodes: Vec<Value>, edges: Vec<Value>) {
    let (status, body) = send(
        app,
        "PUT",
        &format!("/api/v1/flows/{id}"),
        Some(token),
        Some(json!({"canvas_nodes": nodes, "canvas_edges": edges})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save canvas: {body}");
}

async fn deploy(app: &Router, token: &str, id: &str) -> (StatusCode, Value) {
    send(
        app,
        "POST",
        &format!("/api/v1/flows/{id}/start"),
        Some(token),
        None,
    )
    .await
}

async fn hook(app: &Router, id: &str, path: &str, bearer: Option<&str>) -> StatusCode {
    send(
        app,
        "POST",
        &format!("/hook/{id}{path}"),
        bearer,
        Some(json!({})),
    )
    .await
    .0
}

/// Two triggers with different auth policies, each wired to its own
/// `http-out` so the response status reveals which branch actually ran.
fn two_trigger_canvas(open_status: u16) -> (Vec<Value>, Vec<Value>) {
    (
        vec![
            trigger("t_open", "/open", json!({"authType": "none"})),
            http_out("out_open", open_status),
            trigger(
                "t_secure",
                "/secure",
                json!({"authType": "bearer", "authToken": "s3cret"}),
            ),
            http_out("out_secure", 202),
            trigger("t_magic", "/magic", json!({"authType": "magic"})),
            http_out("out_magic", 203),
        ],
        vec![
            edge("t_open", "out_open"),
            edge("t_secure", "out_secure"),
            edge("t_magic", "out_magic"),
        ],
    )
}

#[tokio::test]
async fn hook_and_flow_ownership_security() {
    let app = test_app().await;
    let alice = register(&app, "alice").await;
    let bob = register(&app, "bob").await;

    let (nodes, edges) = two_trigger_canvas(201);
    let flow = create_flow(&app, &alice, nodes, edges).await;
    let (status, body) = deploy(&app, &alice, &flow).await;
    assert_eq!(status, StatusCode::OK, "deploy: {body}");
    assert_eq!(body["status"], "deployed");

    // ── A-01: each route enforces its OWN trigger's policy and runs only
    // that trigger's branch.
    assert_eq!(
        hook(&app, &flow, "/secure", None).await,
        StatusCode::UNAUTHORIZED,
        "anonymous call to the bearer-protected route must be rejected, \
         even though the first trigger on the canvas has no auth"
    );
    assert_eq!(
        hook(&app, &flow, "/secure", Some("wrong")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        hook(&app, &flow, "/secure", Some("s3cret")).await,
        StatusCode::ACCEPTED,
        "valid bearer must run the secure branch (http-out 202)"
    );
    assert_eq!(
        hook(&app, &flow, "/open", None).await,
        StatusCode::CREATED,
        "open route must run only the open branch (http-out 201)"
    );
    assert_eq!(
        hook(&app, &flow, "/magic", None).await,
        StatusCode::INTERNAL_SERVER_ERROR,
        "unknown authType must fail closed, not let the request through"
    );

    // ── A-05: editing the canvas does not change the deployed version.
    let (nodes, edges) = two_trigger_canvas(299);
    save_canvas(&app, &alice, &flow, nodes, edges).await;
    assert_eq!(
        hook(&app, &flow, "/open", None).await,
        StatusCode::CREATED,
        "hooks must keep running the deployed snapshot until redeploy"
    );
    let (status, _) = deploy(&app, &alice, &flow).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        hook(&app, &flow, "/open", None).await.as_u16(),
        299,
        "after redeploy the new version runs"
    );

    // ── A-02: another user can neither stop nor probe the flow.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/flows/{flow}/stop"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "bob must not stop alice's flow"
    );
    assert_eq!(
        hook(&app, &flow, "/open", None).await.as_u16(),
        299,
        "bob's attempt must not have undeployed the flow"
    );

    // The owner can stop it, and stopping retires the public hooks.
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/flows/{flow}/stop"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        hook(&app, &flow, "/open", None).await,
        StatusCode::NOT_FOUND,
        "a stopped flow must stop accepting webhooks"
    );

    // ── Duplicate method + path across triggers is an ambiguous deploy.
    let dup = create_flow(
        &app,
        &alice,
        vec![
            trigger("a", "/same", json!({"authType": "none"})),
            trigger("b", "/same", json!({"authType": "none"})),
        ],
        vec![],
    )
    .await;
    let (status, body) = deploy(&app, &alice, &dup).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicate routes: {body}");
}
