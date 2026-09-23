//! The native node set. Every node but `lance-materialize` maps an envelope
//! to an envelope; configuration (names, labels) is resolved to ids ONCE, when
//! the node is configured.
//!
//! | node type | on a plan handle | on a result handle |
//! |---|---|---|
//! | `lance-source` | — (emits a fresh plan over the named source) | — |
//! | `lance-filter` | AND a selection | refused |
//! | `lance-axis` | add / re-role one coordinate | refused |
//! | `lance-measure` | add a measure | refused |
//! | `lance-pivot` | rewrite roles (metadata) | re-view the SAME cells (metadata) |
//! | `lance-top-k` | set result ordering | re-view with the ordering |
//! | `lance-execute` | fold (or reuse a cached fold) → result | refused |
//! | `lance-materialize` | refused | the data-export terminal: JSON / CSV |

use std::sync::Arc;

use lance_graph_report::render::Terminal;
use lance_graph_report::{
    AxisRole, CmpOp, CoordSpec, FieldId, MaskId, Measure, MeasureKind, ReportPlan, RowRange,
    Scalar, Selection, SourceId, SourceRef, TopK,
};
use serde_json::{json, Value};
use z8run_core::engine::{FlowEngine, NodeExecutor, NodeExecutorFactory};
use z8run_core::{FlowMessage, Z8Error, Z8Result};

use crate::registry::{Envelope, LanceRegistry, Role};

fn err(m: impl Into<String>) -> Z8Error {
    Z8Error::Internal(m.into())
}

/// What a configured node does. Ids only: every name was resolved at
/// configuration.
#[derive(Debug, Clone)]
enum Op {
    Source(SourceId),
    Filter(Selection),
    Axis(CoordSpec, AxisRole),
    Measure(Measure),
    Pivot {
        rows: Vec<CoordSpec>,
        columns: Vec<CoordSpec>,
        pages: Vec<CoordSpec>,
    },
    Rotate,
    TopK(TopK),
    Execute,
    Materialize(Format),
}

#[derive(Debug, Clone, Copy)]
enum Format {
    Json,
    Csv,
}

struct Resolver<'a> {
    reg: &'a LanceRegistry,
}

impl Resolver<'_> {
    fn field(&self, name: &str) -> Z8Result<FieldId> {
        self.reg
            .catalog
            .read()
            .map_err(|_| err("catalog lock poisoned"))?
            .field(name)
            .ok_or_else(|| err(format!("unknown lance field '{name}'")))
    }

    fn field_of(&self, cfg: &Value, key: &str) -> Z8Result<FieldId> {
        let name = cfg
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| err(format!("missing '{key}'")))?;
        self.field(name)
    }

    fn coord(&self, v: &Value) -> Z8Result<CoordSpec> {
        match v {
            Value::String(name) => Ok(CoordSpec::Field(self.field(name)?)),
            Value::Object(_) => {
                let field = self.field_of(v, "field")?;
                match v.get("bucket") {
                    None => Ok(CoordSpec::Field(field)),
                    Some(b) => {
                        let n = |k: &str| {
                            b.get(k)
                                .and_then(Value::as_i64)
                                .ok_or_else(|| err(format!("bucket needs '{k}'")))
                        };
                        Ok(CoordSpec::Bucket {
                            field,
                            origin: n("origin")?,
                            width: n("width")?,
                            count: u32::try_from(n("count")?).map_err(|_| err("bucket count"))?,
                        })
                    }
                }
            }
            _ => Err(err("a coordinate is a field name or {field, bucket?}")),
        }
    }

    fn coords(&self, cfg: &Value, key: &str) -> Z8Result<Vec<CoordSpec>> {
        cfg.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|c| self.coord(c)).collect())
            .unwrap_or(Ok(Vec::new()))
    }

    /// A literal: `value` (signed), `ordinal`, or `label` (resolved via CAM now).
    fn scalar(&self, field: FieldId, v: &Value) -> Z8Result<Scalar> {
        if let Some(l) = v.as_str() {
            let cam = self.reg.cam.read().map_err(|_| err("cam lock poisoned"))?;
            return cam
                .ordinal(field, l)
                .map(Scalar::Ordinal)
                .ok_or_else(|| err(format!("label '{l}' has no ordinal in {field}")));
        }
        v.as_i64()
            .map(Scalar::Int)
            .ok_or_else(|| err("literal must be int or label"))
    }

    fn selection(&self, cfg: &Value) -> Z8Result<Selection> {
        let op = cfg.get("op").and_then(Value::as_str).unwrap_or("eq");
        let sel = match op {
            "range" => {
                let n = |k: &str| {
                    cfg.get(k)
                        .and_then(Value::as_u64)
                        .and_then(|x| u32::try_from(x).ok())
                        .ok_or_else(|| err(format!("range needs '{k}'")))
                };
                Selection::Range(RowRange {
                    lo: n("lo")?,
                    hi: n("hi")?,
                })
            }
            "mask" => Selection::Mask(MaskId(
                cfg.get("mask")
                    .and_then(Value::as_u64)
                    .and_then(|x| u32::try_from(x).ok())
                    .ok_or_else(|| err("mask needs an id"))?,
            )),
            "in" => {
                let f = self.field_of(cfg, "field")?;
                let vals = cfg
                    .get("values")
                    .and_then(Value::as_array)
                    .ok_or_else(|| err("'in' needs 'values'"))?;
                Selection::is_in(
                    f,
                    vals.iter()
                        .map(|v| self.scalar(f, v))
                        .collect::<Z8Result<Vec<_>>>()?,
                )
            }
            cmp => {
                let f = self.field_of(cfg, "field")?;
                let op = match cmp {
                    "eq" => CmpOp::Eq,
                    "ne" => CmpOp::Ne,
                    "lt" => CmpOp::Lt,
                    "le" => CmpOp::Le,
                    "gt" => CmpOp::Gt,
                    "ge" => CmpOp::Ge,
                    other => return Err(err(format!("unknown filter op '{other}'"))),
                };
                let v = cfg
                    .get("value")
                    .ok_or_else(|| err("filter needs 'value'"))?;
                Selection::cmp(f, op, self.scalar(f, v)?)
            }
        };
        Ok(if cfg.get("not").and_then(Value::as_bool) == Some(true) {
            Selection::All.and_not(sel)
        } else {
            sel
        })
    }

    fn op(&self, node_type: &str, cfg: &Value) -> Z8Result<Op> {
        let role = |v: Option<&Value>| match v.and_then(Value::as_str).unwrap_or("row") {
            "row" => Ok(AxisRole::Row),
            "column" => Ok(AxisRole::Column),
            "page" => Ok(AxisRole::Page),
            o => Err(err(format!("unknown role '{o}'"))),
        };
        Ok(match node_type {
            "lance-source" => Op::Source(
                self.reg.source_id(
                    cfg.get("source")
                        .and_then(Value::as_str)
                        .ok_or_else(|| err("source needs a name"))?,
                )?,
            ),
            "lance-filter" => Op::Filter(self.selection(cfg)?),
            "lance-axis" => Op::Axis(
                self.coord(cfg.get("coord").ok_or_else(|| err("axis needs 'coord'"))?)?,
                role(cfg.get("role"))?,
            ),
            "lance-measure" => {
                let kind = match cfg.get("op").and_then(Value::as_str).unwrap_or("count") {
                    "count" => return Ok(Op::Measure(Measure::count())),
                    "sum" => MeasureKind::Sum,
                    "min" => MeasureKind::Min,
                    "max" => MeasureKind::Max,
                    "mean" | "avg" => MeasureKind::Mean,
                    o => return Err(err(format!("unknown measure '{o}'"))),
                };
                Op::Measure(Measure::of(kind, self.field_of(cfg, "field")?))
            }
            "lance-pivot" => {
                if cfg.get("rotate").and_then(Value::as_bool) == Some(true) {
                    Op::Rotate
                } else {
                    Op::Pivot {
                        rows: self.coords(cfg, "rows")?,
                        columns: self.coords(cfg, "columns")?,
                        pages: self.coords(cfg, "pages")?,
                    }
                }
            }
            "lance-top-k" => Op::TopK(TopK {
                measure: cfg.get("measure").and_then(Value::as_u64).unwrap_or(0) as usize,
                k: cfg
                    .get("k")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| err("top-k needs 'k'"))? as usize,
                descending: cfg
                    .get("descending")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            }),
            "lance-execute" => Op::Execute,
            "lance-materialize" => {
                Op::Materialize(
                    match cfg.get("format").and_then(Value::as_str).unwrap_or("json") {
                        "json" => Format::Json,
                        "csv" => Format::Csv,
                        // Paged / screen output is not the reporting engine's: a
                        // report becomes a paper through OGAR composition (an
                        // ObjectSlot naming the result; lance-graph-report-ogar).
                        "html" | "typst" | "pdf" => return Err(err(
                            "lance-materialize exports data (json/csv); paged or screen output \
                             goes through OGAR composition (an ObjectSlot on the report, see \
                             lance-graph-report-ogar)",
                        )),
                        o => return Err(err(format!("unknown format '{o}'"))),
                    },
                )
            }
            other => return Err(err(format!("not a lance node type '{other}'"))),
        })
    }
}

/// One configured lance node.
pub struct LanceNode {
    node_type: &'static str,
    op: Op,
    reg: Arc<LanceRegistry>,
}

impl LanceNode {
    fn emit(&self, msg: &FlowMessage, env: Envelope) -> Vec<FlowMessage> {
        vec![msg.derive(msg.source_node, "output", env.to_json())]
    }

    fn step(&self, msg: &FlowMessage) -> Z8Result<Vec<FlowMessage>> {
        if let Op::Source(id) = self.op {
            let b = self.reg.batch(id)?;
            let env = self.reg.put_plan(ReportPlan::over(SourceRef {
                id,
                generation: b.generation(),
            }))?;
            return Ok(self.emit(msg, env));
        }
        let env = Envelope::from_json(&msg.payload)?;
        match (env.role, &self.op) {
            (
                Role::Result,
                Op::Pivot {
                    rows,
                    columns,
                    pages,
                },
            ) => {
                let r = self.reg.result(&env)?;
                let r = r
                    .with_roles(rows, columns, pages)
                    .map_err(|e| err(e.to_string()))?;
                self.reg
                    .stats
                    .reinterpretations
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let plan_like = ReportPlan::over(SourceRef {
                    id: env.source,
                    generation: env.generation,
                });
                Ok(self.emit(msg, self.reg.put_result(&plan_like, r)?))
            }
            (Role::Result, Op::Rotate) => {
                let r = self.reg.result(&env)?.rotate();
                self.reg
                    .stats
                    .reinterpretations
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let plan_like = ReportPlan::over(SourceRef {
                    id: env.source,
                    generation: env.generation,
                });
                Ok(self.emit(msg, self.reg.put_result(&plan_like, r)?))
            }
            (Role::Result, Op::TopK(t)) => {
                let r = self.reg.result(&env)?;
                let m = r
                    .measures()
                    .get(t.measure)
                    .ok_or_else(|| err("top-k measure out of range"))?
                    .clone();
                let r = r.order_rows_by(&m, t.descending, Some(t.k));
                let plan_like = ReportPlan::over(SourceRef {
                    id: env.source,
                    generation: env.generation,
                });
                Ok(self.emit(msg, self.reg.put_result(&plan_like, r)?))
            }
            (Role::Result, Op::Materialize(fmt)) => {
                let r = self.reg.result(&env)?;
                let cam = self.reg.cam.read().map_err(|_| err("cam lock poisoned"))?;
                let kv = self.reg.kv.read().map_err(|_| err("kv lock poisoned"))?;
                let cat = self
                    .reg
                    .catalog
                    .read()
                    .map_err(|_| err("catalog lock poisoned"))?;
                let t = Terminal {
                    cam: &cam,
                    kv: &kv,
                    catalog: &cat,
                };
                let payload = match fmt {
                    Format::Json => {
                        let (s, _) = t.json(&r);
                        json!({ "format": "json", "body": serde_json::from_str::<Value>(&s).map_err(|e| err(e.to_string()))? })
                    }
                    Format::Csv => json!({ "format": "csv", "body": t.csv(&r).0 }),
                };
                Ok(vec![msg.derive(msg.source_node, "output", payload)])
            }
            (Role::Result, _) => Err(err(format!(
                "{} does not accept a result handle",
                self.node_type
            ))),
            (Role::Plan, Op::Materialize(_)) => Err(err(
                "lance-materialize needs a result handle; place lance-execute before it",
            )),
            (Role::Plan, Op::Execute) => {
                let plan = self.reg.plan(&env)?;
                let res = self.reg.execute(&plan)?;
                let out = self.reg.put_result(&plan, res)?;
                let explain = self
                    .reg
                    .batch(plan.source.id)
                    .and_then(|b| {
                        plan.explain(&b, &self.reg.policy)
                            .map_err(|e| err(e.to_string()))
                    })
                    .map(|p| p.to_string())
                    .unwrap_or_default();
                Ok(vec![msg
                    .derive(msg.source_node, "output", out.to_json())
                    .with_metadata("lance.physical", Value::String(explain))])
            }
            (Role::Plan, op) => {
                let plan = (*self.reg.plan(&env)?).clone();
                let plan = match op.clone() {
                    Op::Filter(s) => plan.filter(s),
                    Op::Axis(c, r) => plan.axis(c, r),
                    Op::Measure(m) => plan.measure(m),
                    Op::Pivot {
                        rows,
                        columns,
                        pages,
                    } => plan.view(&rows, &columns, &pages),
                    Op::Rotate => plan.rotate(),
                    Op::TopK(t) => plan.top_k(t),
                    Op::Source(_) | Op::Execute | Op::Materialize(_) => {
                        unreachable!("handled above")
                    }
                };
                Ok(self.emit(msg, self.reg.put_plan(plan)?))
            }
        }
    }
}

#[async_trait::async_trait]
impl NodeExecutor for LanceNode {
    async fn process(&self, msg: FlowMessage) -> Z8Result<Vec<FlowMessage>> {
        self.step(&msg)
    }

    async fn configure(&mut self, config: Value) -> Z8Result<()> {
        self.op = Resolver { reg: &self.reg }.op(self.node_type, &config)?;
        Ok(())
    }

    async fn validate(&self) -> Z8Result<()> {
        Ok(())
    }

    fn node_type(&self) -> &str {
        self.node_type
    }
}

/// Factory for one lance node type, sharing the registry.
pub struct LanceNodeFactory {
    node_type: &'static str,
    reg: Arc<LanceRegistry>,
}

#[async_trait::async_trait]
impl NodeExecutorFactory for LanceNodeFactory {
    async fn create(&self, config: Value) -> Z8Result<Box<dyn NodeExecutor>> {
        let mut n = LanceNode {
            node_type: self.node_type,
            op: Op::Execute,
            reg: Arc::clone(&self.reg),
        };
        n.configure(config).await?;
        Ok(Box::new(n))
    }

    fn node_type(&self) -> &str {
        self.node_type
    }
}

/// Every lance node type.
pub const NODE_TYPES: [&str; 8] = [
    "lance-source",
    "lance-filter",
    "lance-axis",
    "lance-measure",
    "lance-pivot",
    "lance-top-k",
    "lance-execute",
    "lance-materialize",
];

/// A factory for one node type (tests and embedders that drive nodes directly).
pub fn factory(node_type: &'static str, reg: &Arc<LanceRegistry>) -> LanceNodeFactory {
    LanceNodeFactory {
        node_type,
        reg: Arc::clone(reg),
    }
}

/// Register every lance node type with an engine.
pub async fn register_lance_nodes(engine: &FlowEngine, reg: Arc<LanceRegistry>) {
    for t in NODE_TYPES {
        engine
            .register_node_type(Arc::new(factory(t, &reg)) as Arc<dyn NodeExecutorFactory>)
            .await;
    }
}
