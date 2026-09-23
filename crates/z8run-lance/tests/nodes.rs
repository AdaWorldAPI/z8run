//! Node-level falsifiers: FlowMessages carry handles, never populations;
//! z8run-built plans are the native plans (TEST 9's z8run leg); pivot on a
//! result is a re-view; a rotated plan reuses the cached fold (F14).

use std::sync::Arc;

use lance_graph_report::boundary::Catalog;
use lance_graph_report::{
    AbiBatch, AxisRole, CmpOp, Column, CoordSpec, FieldId, LaneData, Measure, MeasureKind,
    PlannerPolicy, ReportPlan, Scalar, Selection, SourceId, SourceRef,
};
use serde_json::{json, Value};
use uuid::Uuid;
use z8run_core::engine::NodeExecutorFactory;
use z8run_core::FlowMessage;
use z8run_lance::nodes::factory;
use z8run_lance::{Envelope, LanceRegistry, RegistryStats};

const A: FieldId = FieldId(1);
const B: FieldId = FieldId(2);
const V: FieldId = FieldId(3);

/// A synthetic source: two coordinate fields over CAM labels, one measure.
fn registry(n: usize) -> Arc<LanceRegistry> {
    let reg = Arc::new(LanceRegistry::new(PlannerPolicy::default()));
    let mut s = 7u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let a_raw: Vec<String> = (0..n).map(|_| format!("a{}", next() % 4)).collect();
    let b_raw: Vec<String> = (0..n).map(|_| format!("b{}", next() % 7)).collect();
    let v: Vec<i32> = (0..n).map(|_| (next() % 100) as i32).collect();
    let (a, b, da, db) = {
        let mut cam = reg.cam.write().unwrap();
        let mut kv = reg.kv.write().unwrap();
        let a = cam.canonicalize(A, a_raw.iter().map(String::as_str), &mut kv);
        let b = cam.canonicalize(B, b_raw.iter().map(String::as_str), &mut kv);
        (a, b, cam.domain(A), cam.domain(B))
    };
    *reg.catalog.write().unwrap() = Catalog::default().with("a", A).with("b", B).with("v", V);
    let batch = AbiBatch::new(SourceId(5), 1, n)
        .with_column(Column::coordinate(A, a, da))
        .unwrap()
        .with_column(Column::coordinate(B, b, db))
        .unwrap()
        .with_column(Column::value(V, LaneData::I32(v.into())))
        .unwrap();
    reg.publish("synthetic", batch).unwrap();
    reg
}

async fn run(
    reg: &Arc<LanceRegistry>,
    node: &'static str,
    cfg: Value,
    msg: FlowMessage,
) -> FlowMessage {
    let n = factory(node, reg).create(cfg).await.unwrap();
    let mut out = n.process(msg).await.unwrap();
    assert_eq!(out.len(), 1);
    out.pop().unwrap()
}

fn start() -> FlowMessage {
    FlowMessage::new(Uuid::now_v7(), "trigger", json!({}), Uuid::now_v7())
}

async fn pivot_flow(reg: &Arc<LanceRegistry>) -> (FlowMessage, Vec<usize>) {
    let mut sizes = Vec::new();
    let mut m = run(reg, "lance-source", json!({"source": "synthetic"}), start()).await;
    for (node, cfg) in [
        (
            "lance-filter",
            json!({"field": "v", "op": "ge", "value": 10}),
        ),
        (
            "lance-filter",
            json!({"field": "a", "op": "ne", "value": "a3"}),
        ),
        ("lance-axis", json!({"coord": "a", "role": "row"})),
        ("lance-axis", json!({"coord": "b", "role": "column"})),
        ("lance-measure", json!({"op": "sum", "field": "v"})),
        ("lance-measure", json!({"op": "count"})),
    ] {
        sizes.push(serde_json::to_vec(&m.payload).unwrap().len());
        m = run(reg, node, cfg, m).await;
    }
    sizes.push(serde_json::to_vec(&m.payload).unwrap().len());
    (m, sizes)
}

#[tokio::test]
async fn flow_messages_carry_handles_not_populations() {
    for n in [1_000usize, 200_000] {
        let reg = registry(n);
        let (plan_msg, mut sizes) = pivot_flow(&reg).await;
        let res = run(&reg, "lance-execute", json!({}), plan_msg).await;
        sizes.push(serde_json::to_vec(&res.payload).unwrap().len());
        // Every hop is a ~100-byte envelope, whatever the population.
        assert!(sizes.iter().all(|&s| s < 128), "{sizes:?} at n={n}");
        assert_eq!(
            Envelope::from_json(&res.payload).unwrap().role,
            z8run_lance::Role::Result
        );
        assert!(res.metadata["lance.physical"]
            .as_str()
            .unwrap()
            .contains("Materialization: terminal only"));
        // Only the explicit terminal produces text, and it is result-sized.
        let out = run(&reg, "lance-materialize", json!({"format": "json"}), res).await;
        let body = &out.payload["body"];
        assert_eq!(body["columns"].as_array().unwrap().len(), 7);
        assert_eq!(body["pages"][0]["rows"].as_array().unwrap().len(), 4);
        // Anti-vacuity: the filtered-out label's row is empty (NULL sum, 0 count).
        let a3 = body["pages"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["row"][0] == "a3")
            .unwrap();
        assert!(a3["total"][0].is_null());
        assert_eq!(a3["total"][1], 0);
    }
}

#[tokio::test]
async fn z8run_plan_is_the_native_plan() {
    let reg = registry(5_000);
    let (plan_msg, _) = pivot_flow(&reg).await;
    let z8 = reg
        .plan(&Envelope::from_json(&plan_msg.payload).unwrap())
        .unwrap();
    let a3 = reg.cam.read().unwrap().ordinal(A, "a3").unwrap();
    let native = ReportPlan::over(SourceRef {
        id: SourceId(5),
        generation: 1,
    })
    .filter(Selection::cmp(V, CmpOp::Ge, Scalar::Int(10)))
    .filter(Selection::cmp(A, CmpOp::Ne, Scalar::Ordinal(a3)))
    .axis(CoordSpec::Field(A), AxisRole::Row)
    .axis(CoordSpec::Field(B), AxisRole::Column)
    .measure(Measure::of(MeasureKind::Sum, V))
    .measure(Measure::count());
    assert_eq!(
        *z8, native,
        "the visual flow builds exactly the native plan"
    );
    let batch = reg.batch(SourceId(5)).unwrap();
    let pol = PlannerPolicy::default();
    assert_eq!(
        z8.explain(&batch, &pol).unwrap().first_pass,
        native.explain(&batch, &pol).unwrap().first_pass
    );
}

#[tokio::test]
async fn pivot_on_a_result_and_rotated_plans_never_refold() {
    let reg = registry(50_000);
    let (plan_msg, _) = pivot_flow(&reg).await;
    let res = run(&reg, "lance-execute", json!({}), plan_msg.clone()).await;
    assert_eq!(RegistryStats::get(&reg.stats.executions), 1);
    let r0 = reg
        .result(&Envelope::from_json(&res.payload).unwrap())
        .unwrap();

    // (a) pivot on the RESULT handle: a re-view, no fold, same cells.
    let rot = run(&reg, "lance-pivot", json!({"rotate": true}), res.clone()).await;
    let r1 = reg
        .result(&Envelope::from_json(&rot.payload).unwrap())
        .unwrap();
    assert!(Arc::ptr_eq(r0.space(), r1.space()));
    assert_eq!(RegistryStats::get(&reg.stats.executions), 1);

    // (b) pivot on the PLAN, then execute: the physical-key cache answers.
    let rp = run(
        &reg,
        "lance-pivot",
        json!({"rows": ["b"], "columns": ["a"]}),
        plan_msg,
    )
    .await;
    let res2 = run(&reg, "lance-execute", json!({}), rp).await;
    let r2 = reg
        .result(&Envelope::from_json(&res2.payload).unwrap())
        .unwrap();
    assert_eq!(
        RegistryStats::get(&reg.stats.executions),
        1,
        "no second fold"
    );
    assert_eq!(RegistryStats::get(&reg.stats.cache_hits), 1);
    assert!(Arc::ptr_eq(r0.space(), r2.space()));
    assert_eq!(r2.view().rows, r0.view().columns);

    // Fan-out: two branches off one plan handle share the plan Arc.
    let e = Envelope::from_json(&res.payload).unwrap();
    assert!(Arc::ptr_eq(r0.space(), reg.result(&e).unwrap().space()));
}

#[tokio::test]
async fn republished_source_invalidates_old_handles() {
    let reg = registry(1_000);
    let (plan_msg, _) = pivot_flow(&reg).await;
    let b = reg.batch(SourceId(5)).unwrap();
    let mut republished = AbiBatch::new(SourceId(5), 2, b.n_rows());
    for c in b.columns() {
        republished = republished.with_column(c.clone()).unwrap();
    }
    reg.publish("synthetic", republished).unwrap();
    let n = factory("lance-execute", &reg)
        .create(json!({}))
        .await
        .unwrap();
    let e = n.process(plan_msg).await.unwrap_err().to_string();
    assert!(e.contains("stale lance handle"), "{e}");
}

#[tokio::test]
async fn materialize_refuses_a_plan_handle() {
    let reg = registry(100);
    let (plan_msg, _) = pivot_flow(&reg).await;
    let n = factory("lance-materialize", &reg)
        .create(json!({"format": "csv"}))
        .await
        .unwrap();
    assert!(n
        .process(plan_msg)
        .await
        .unwrap_err()
        .to_string()
        .contains("needs a result handle"));
}
