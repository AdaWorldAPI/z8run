//! PostgreSQL storage tests (R-05). They run when `Z8_TEST_POSTGRES_URL`
//! points at a disposable database (the CI job starts one) and are skipped
//! otherwise. The database is wiped, so its name must contain "test".

use tokio::sync::OnceCell;
use uuid::Uuid;
use z8run_core::flow::Flow;
use z8run_storage::postgres::PgStorage;
use z8run_storage::repository::{
    ExecutionRepository, FlowRepository, HookMatch, HookRoute, UserRecord, UserRepository,
};

static RESET: OnceCell<()> = OnceCell::const_new();

/// A migrated PostgreSQL storage, or `None` when no test database is set.
async fn storage() -> Option<PgStorage> {
    let url = std::env::var("Z8_TEST_POSTGRES_URL").ok()?;
    let database = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    assert!(
        database.contains("test"),
        "refusing to wipe '{database}': Z8_TEST_POSTGRES_URL must name a test database"
    );
    // Empty schema and migrations once per test binary: tests run in
    // parallel, and concurrent CREATE TABLE IF NOT EXISTS races in Postgres.
    RESET
        .get_or_init(|| async {
            let pool = sqlx::PgPool::connect(&url).await.expect("connect");
            sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
                .execute(&pool)
                .await
                .expect("reset schema");
            PgStorage::new(&url)
                .await
                .expect("connect")
                .migrate()
                .await
                .expect("migrate");
        })
        .await;
    Some(PgStorage::new(&url).await.expect("connect"))
}

macro_rules! pg_or_skip {
    () => {
        match storage().await {
            Some(pg) => pg,
            None => {
                eprintln!("skipped: Z8_TEST_POSTGRES_URL is not set");
                return;
            }
        }
    };
}

async fn owner(pg: &PgStorage) -> Uuid {
    let id = Uuid::now_v7();
    pg.create_user(&UserRecord {
        id,
        email: format!("{id}@example.com"),
        username: format!("u{}", id.simple()),
        password_hash: "x".to_string(),
        roles: vec!["user".to_string()],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    })
    .await
    .unwrap();
    id
}

fn route(method: &str, path: &str, node_id: &str) -> HookRoute {
    HookRoute {
        method: method.to_string(),
        path: path.to_string(),
        node_id: node_id.to_string(),
        node_type: "webhook-trigger".to_string(),
    }
}

fn matched(owner: Uuid, node_id: &str) -> Option<HookMatch> {
    Some(HookMatch {
        user_id: owner,
        node_id: node_id.to_string(),
    })
}

#[tokio::test]
async fn deploy_redeploy_and_undeploy() {
    let pg = pg_or_skip!();
    let owner = owner(&pg).await;
    let mut flow = Flow::new("original");
    pg.save_flow_with_user(&flow, owner).await.unwrap();

    pg.deploy_flow(
        flow.id,
        owner,
        &flow,
        &[route("POST", "/a", "n1"), route("GET", "/b", "n2")],
    )
    .await
    .unwrap();
    assert_eq!(
        pg.find_hook_route(flow.id, "POST", "/a").await.unwrap(),
        matched(owner, "n1")
    );
    assert_eq!(
        pg.find_hook_route(flow.id, "GET", "/a").await.unwrap(),
        None
    );

    // Edits after deploy don't change the snapshot (A-05).
    flow.name = "edited".to_string();
    pg.save_flow(&flow).await.unwrap();
    assert_eq!(
        pg.get_deployment(flow.id).await.unwrap().map(|f| f.name),
        Some("original".to_string())
    );

    // A redeploy replaces routes and snapshot atomically.
    pg.deploy_flow(flow.id, owner, &flow, &[route("POST", "/c", "n3")])
        .await
        .unwrap();
    assert_eq!(
        pg.find_hook_route(flow.id, "POST", "/a").await.unwrap(),
        None
    );
    assert_eq!(
        pg.find_hook_route(flow.id, "POST", "/c").await.unwrap(),
        matched(owner, "n3")
    );
    assert_eq!(
        pg.get_deployment(flow.id).await.unwrap().map(|f| f.name),
        Some("edited".to_string())
    );

    // Undeploy removes both.
    pg.undeploy_flow(flow.id).await.unwrap();
    assert_eq!(
        pg.find_hook_route(flow.id, "POST", "/c").await.unwrap(),
        None
    );
    assert!(pg.get_deployment(flow.id).await.unwrap().is_none());
}

#[tokio::test]
async fn deleting_a_flow_retires_its_hooks() {
    let pg = pg_or_skip!();
    let owner = owner(&pg).await;
    let flow = Flow::new("to delete");
    pg.save_flow_with_user(&flow, owner).await.unwrap();
    pg.deploy_flow(flow.id, owner, &flow, &[route("POST", "/x", "n1")])
        .await
        .unwrap();

    pg.delete_flow_for_user(flow.id, owner).await.unwrap();
    assert_eq!(
        pg.find_hook_route(flow.id, "POST", "/x").await.unwrap(),
        None
    );
    assert!(pg.get_deployment(flow.id).await.unwrap().is_none());
}

#[tokio::test]
async fn execution_terminal_states_and_startup_reconciliation() {
    let pg = pg_or_skip!();
    let owner = owner(&pg).await;
    let flow = Flow::new("runs");
    pg.save_flow_with_user(&flow, owner).await.unwrap();
    let status = |id: Uuid| {
        let pg = &pg;
        async move {
            pg.get_history(flow.id, 10)
                .await
                .unwrap()
                .into_iter()
                .find(|e| e.id == id)
                .map(|e| (e.status, e.error))
                .unwrap()
        }
    };

    let stopped = pg.record_start(flow.id, Uuid::now_v7()).await.unwrap();
    pg.record_completion(stopped, "stopped", 5, None)
        .await
        .unwrap();
    pg.record_completion(stopped, "completed", 9, None)
        .await
        .unwrap();
    assert_eq!(status(stopped).await.0, "stopped", "terminal state sticks");

    let stale = pg.record_start(flow.id, Uuid::now_v7()).await.unwrap();
    let before = chrono::Utc::now() - chrono::Duration::hours(1);
    assert_eq!(pg.interrupt_running(before, "x").await.unwrap(), 0);
    let after = chrono::Utc::now() + chrono::Duration::seconds(1);
    assert!(pg.interrupt_running(after, "interrupted").await.unwrap() >= 1);
    assert_eq!(
        status(stale).await,
        ("stopped".to_string(), Some("interrupted".to_string()))
    );
}
