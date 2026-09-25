//! Node-level falsifiers: FlowMessages carry handles, never populations;
//! z8run-built plans are the native plans (TEST 9's z8run leg); pivot on a
//! result is a re-view; a rotated plan reuses the cached fold (F14).

use std::sync::Arc;

use lance_graph_report::boundary::Catalog;
use lance_graph_report::render::{Grid, Terminal};
use lance_graph_report::{
    AbiBatch, AxisRole, CellValue, CmpOp, Column, CoordSpec, FieldId, LaneData, Measure,
    MeasureKind, PlannerPolicy, ReportPlan, Scalar, Selection, SourceId, SourceRef,
};
use serde_json::{json, Value};
use uuid::Uuid;
use z8run_core::engine::NodeExecutorFactory;
use z8run_core::FlowMessage;
use z8run_lance::nodes::{factory, grid_value};
use z8run_lance::{Envelope, LanceRegistry, RegistryStats};

const A: FieldId = FieldId(1);
const B: FieldId = FieldId(2);
const V: FieldId = FieldId(3);

/// A synthetic source: two coordinate fields over CAM labels, one measure.
fn registry(n: usize) -> Arc<LanceRegistry> {
    registry_labelled(n, &["a0", "a1", "a2", "a3"])
}

/// As [`registry`], with field `a`'s labels drawn from `a_labels`.
fn registry_labelled(n: usize, a_labels: &[&str]) -> Arc<LanceRegistry> {
    let reg = Arc::new(LanceRegistry::new(PlannerPolicy::default()));
    let mut s = 7u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let a_raw: Vec<String> = (0..n)
        .map(|_| a_labels[(next() % a_labels.len() as u64) as usize].to_string())
        .collect();
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

#[tokio::test]
async fn paged_output_is_refused_and_points_at_ogar_composition() {
    let reg = registry(100);
    let e = match factory("lance-materialize", &reg)
        .create(json!({"format": "html"}))
        .await
    {
        Err(e) => e.to_string(),
        Ok(_) => panic!("html must not configure"),
    };
    assert!(e.contains("OGAR composition"), "{e}");
    // Stay-silent twin: the data-export formats still configure.
    for f in ["json", "csv"] {
        assert!(factory("lance-materialize", &reg)
            .create(json!({ "format": f }))
            .await
            .is_ok());
    }
}

/// The document the textual export path produced: `Terminal::json` text,
/// parsed. The oracle the direct `Value` path must equal.
fn textual_body(reg: &LanceRegistry, res: &FlowMessage) -> Value {
    let r = reg
        .result(&Envelope::from_json(&res.payload).unwrap())
        .unwrap();
    let (cam, kv, catalog) = (
        reg.cam.read().unwrap(),
        reg.kv.read().unwrap(),
        reg.catalog.read().unwrap(),
    );
    let t = Terminal {
        cam: &cam,
        kv: &kv,
        catalog: &catalog,
    };
    serde_json::from_str(&t.json(&r).0).unwrap()
}

/// Structural equality of the direct document against the textual one:
/// same keys, same strings, same integers, same nulls — and every float equal
/// or one ulp away, because the textual path's read-back is not
/// correctly rounded (see `real_cells_are_exact_where_the_text_path_drifted`).
/// Returns how many float cells were compared.
fn same_document(new: &Value, old: &Value, at: &str) -> usize {
    match (new, old) {
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{at}"
            );
            a.iter()
                .map(|(k, v)| same_document(v, &b[k], &format!("{at}.{k}")))
                .sum()
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{at}");
            a.iter()
                .zip(b)
                .enumerate()
                .map(|(i, (x, y))| same_document(x, y, &format!("{at}[{i}]")))
                .sum()
        }
        (Value::Number(a), Value::Number(b)) if a.is_f64() || b.is_f64() => {
            assert!(a.is_f64() && b.is_f64(), "{at}: number kind {a} vs {b}");
            let (x, y) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            assert!(
                x.to_bits().abs_diff(y.to_bits()) <= 1,
                "{at}: {x} vs textual {y}"
            );
            1
        }
        _ => {
            assert_eq!(new, old, "{at}");
            0
        }
    }
}

#[tokio::test]
async fn json_export_is_the_textual_document_without_the_text() {
    // Labels that exercise every escape `Terminal`'s JSON writer has.
    let labels = [
        "plain",
        "quo\"te",
        "back\\slash",
        "new\nline",
        "ctl\u{1}",
        "ümlaut ✓",
    ];
    let reg = registry_labelled(3_000, &labels);
    let mut m = run(
        &reg,
        "lance-source",
        json!({"source": "synthetic"}),
        start(),
    )
    .await;
    for (node, cfg) in [
        (
            "lance-filter",
            json!({"field": "v", "op": "ge", "value": 60}),
        ),
        (
            "lance-filter",
            json!({"field": "a", "op": "ne", "value": "plain"}),
        ),
        ("lance-axis", json!({"coord": "a", "role": "row"})),
        ("lance-axis", json!({"coord": "b", "role": "column"})),
        ("lance-measure", json!({"op": "sum", "field": "v"})),
        ("lance-measure", json!({"op": "count"})),
        ("lance-measure", json!({"op": "mean", "field": "v"})),
    ] {
        m = run(&reg, node, cfg, m).await;
    }
    let res = run(&reg, "lance-execute", json!({}), m).await;
    let out = run(
        &reg,
        "lance-materialize",
        json!({"format": "json"}),
        res.clone(),
    )
    .await;
    let body = &out.payload["body"];
    assert_eq!(out.payload["format"], "json");
    let floats = same_document(body, &textual_body(&reg, &res), "$");
    assert!(floats > 0, "the comparison must reach real cells");

    // Anti-vacuity: the document carries every value kind and every label.
    let flat = body.to_string();
    let cells: Vec<&Value> = body["pages"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["cells"].as_array().unwrap().iter())
        .flat_map(|c| c.as_array().unwrap().iter())
        .collect();
    assert!(cells.iter().any(|v| v.is_i64() || v.is_u64()), "{flat}");
    assert!(cells.iter().any(|v| v.is_f64()), "{flat}");
    assert!(cells.iter().any(|v| v.is_null()), "{flat}");
    let rows: Vec<&str> = body["pages"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["row"][0].as_str().unwrap())
        .collect();
    for l in labels {
        assert!(rows.contains(&l), "{l:?} missing from {rows:?}");
    }
}

fn grand_total(vals: &[CellValue]) -> Value {
    let g = Grid {
        column_keys: vec![],
        columns: vec![],
        measures: vec![],
        pages: vec![],
        grand_total: vals.to_vec(),
    };
    grid_value(&g).unwrap()["grand_total"].clone()
}

/// The textual spelling `Terminal` writes for a cell (render.rs `fmt_value` /
/// `json_value`).
fn spelled(v: CellValue) -> String {
    match v {
        CellValue::Int(n) => n.to_string(),
        CellValue::Real(r) => format!("{r}"),
        CellValue::Null => "null".to_string(),
    }
}

#[test]
fn real_cells_keep_the_number_kind_and_value_of_the_textual_path() {
    let reals = [
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        -2.25,
        0.1,
        12_345.678,
        1e15,
        -1e15,
        9_007_199_254_740_992.0,
        -9_007_199_254_740_992.0,
        1e21,
        1e22,
        -1e22,
        1e300,
        -1e300,
        5e-324,
        f64::EPSILON,
    ];
    let mut vals: Vec<CellValue> = reals.iter().map(|&r| CellValue::Real(r)).collect();
    vals.extend([
        CellValue::Int(0),
        CellValue::Int(-7),
        CellValue::Int(i64::MIN),
        CellValue::Int(i64::MAX),
        CellValue::Null,
    ]);
    let got = grand_total(&vals);
    for (i, &v) in vals.iter().enumerate() {
        let text = spelled(v);
        let want: Value = serde_json::from_str(&text).unwrap();
        // Same value AND same integer/float kind as the textual document.
        assert_eq!(got[i], want, "cell {v:?} spelled {text}");
        assert_eq!(got[i].is_f64(), want.is_f64(), "kind of {v:?}");
        if let CellValue::Real(r) = v {
            assert_eq!(got[i].as_f64(), Some(r), "exact value of {v:?}");
        }
    }
}

#[test]
fn real_cells_are_exact_where_the_text_path_drifted() {
    // The textual path read `Display` text back with a JSON reader that is not
    // correctly rounded: this mean-like value came back one ulp off (about 12%
    // of a/b ratios do), and the extremes did not come back at all ("number
    // out of range"). The direct path carries the cell's own f64.
    let drift = 1121.2179199154223_f64;
    let reread: Value = serde_json::from_str(&format!("{drift}")).unwrap();
    assert_ne!(
        reread.as_f64(),
        Some(drift),
        "fixture: the text path drifted"
    );
    assert_eq!(
        grand_total(&[CellValue::Real(drift)])[0].as_f64(),
        Some(drift)
    );
    for r in [f64::MAX, f64::MIN] {
        assert!(serde_json::from_str::<Value>(&format!("{r}")).is_err());
        assert_eq!(grand_total(&[CellValue::Real(r)])[0].as_f64(), Some(r));
    }
    // Above 2^53 an integral f64's `Display` is its shortest round-trip
    // digits padded with zeros, so the text path read 2^63 back as the
    // DIFFERENT integer 9223372036854776000. The direct path carries 2^63.
    let two_63 = 9_223_372_036_854_775_808.0_f64;
    let reread: Value = serde_json::from_str(&format!("{two_63}")).unwrap();
    assert_eq!(reread.as_u64(), Some(9_223_372_036_854_776_000));
    assert_eq!(
        grand_total(&[CellValue::Real(two_63)])[0].as_u64(),
        Some(1u64 << 63)
    );
}

#[test]
fn non_finite_cells_are_refused_as_the_text_path_refused_them() {
    for r in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(serde_json::from_str::<Value>(&format!("{r}")).is_err());
        let g = Grid {
            column_keys: vec![],
            columns: vec![],
            measures: vec![],
            pages: vec![],
            grand_total: vec![CellValue::Real(r)],
        };
        assert!(grid_value(&g).is_err(), "{r} must not become a JSON value");
    }
    // Stay-silent twin: a finite real passes.
    let g = Grid {
        column_keys: vec![],
        columns: vec![],
        measures: vec![],
        pages: vec![],
        grand_total: vec![CellValue::Real(2.5)],
    };
    assert!(grid_value(&g).is_ok());
}
