//! The handle registry: where published sources, immutable plans and computed
//! results live while a flow refers to them by number.
//!
//! A handle is `(role, handle, generation, source)`. A source republished
//! under a new generation invalidates every handle minted against the old
//! one — resolution fails closed rather than answering from stale lanes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lance_graph_report::boundary::{CamLabels, Catalog, MemKv};
use lance_graph_report::{
    AbiBatch, PhysicalKey, PlannerPolicy, ReportPlan, ReportResult, SourceId,
};
use serde_json::{json, Value};
use z8run_core::{Z8Error, Z8Result};

/// What a handle names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// An immutable report plan.
    Plan,
    /// A computed report result (shared aggregate space + view).
    Result,
}

/// The tiny control-plane envelope a `FlowMessage` payload carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Envelope {
    /// Plan or result.
    pub role: Role,
    /// Registry handle.
    pub handle: u64,
    /// Source generation it was minted against.
    pub generation: u32,
    /// Source identity.
    pub source: SourceId,
}

impl Envelope {
    /// Render as the payload JSON.
    pub fn to_json(self) -> Value {
        json!({
            "$kind": "lance-abi",
            "role": match self.role { Role::Plan => "plan", Role::Result => "result" },
            "handle": self.handle,
            "generation": self.generation,
            "source": self.source.0,
        })
    }

    /// Parse a payload JSON.
    pub fn from_json(v: &Value) -> Z8Result<Self> {
        let bad = || Z8Error::Internal("expected a {\"$kind\":\"lance-abi\"} envelope".into());
        if v.get("$kind").and_then(Value::as_str) != Some("lance-abi") {
            return Err(bad());
        }
        let role = match v.get("role").and_then(Value::as_str) {
            Some("plan") => Role::Plan,
            Some("result") => Role::Result,
            _ => return Err(bad()),
        };
        let num = |k| v.get(k).and_then(Value::as_u64).ok_or_else(bad);
        Ok(Self {
            role,
            handle: num("handle")?,
            generation: u32::try_from(num("generation")?).map_err(|_| bad())?,
            source: SourceId(u32::try_from(num("source")?).map_err(|_| bad())?),
        })
    }
}

/// Registry counters (the node-level falsifiers read these).
#[derive(Debug, Default)]
pub struct RegistryStats {
    /// Folds actually executed.
    pub executions: AtomicU64,
    /// `lance-execute` answered from the physical-key cache.
    pub cache_hits: AtomicU64,
    /// Results re-viewed without any fold (pivot on a result handle).
    pub reinterpretations: AtomicU64,
}

impl RegistryStats {
    /// Read a counter.
    pub fn get(c: &AtomicU64) -> u64 {
        c.load(Ordering::Relaxed)
    }
}

struct Inner {
    sources: HashMap<SourceId, Arc<AbiBatch>>,
    source_names: Vec<(String, SourceId)>,
    plans: HashMap<u64, Arc<ReportPlan>>,
    results: HashMap<u64, ReportResult>,
    cache: HashMap<PhysicalKey, ReportResult>,
    next: u64,
}

/// Shared by every lance node of an engine. The boundary stores (catalog,
/// CAM, KV) sit beside the substrate, never inside a plan.
pub struct LanceRegistry {
    inner: RwLock<Inner>,
    /// Field-name resolution (adapter boundary).
    pub catalog: RwLock<Catalog>,
    /// CAM label codebook (adapter boundary / terminal).
    pub cam: RwLock<CamLabels>,
    /// Raw-value KV (terminal dereference only).
    pub kv: RwLock<MemKv>,
    /// Physical planner policy.
    pub policy: PlannerPolicy,
    /// Counters.
    pub stats: RegistryStats,
}

fn poisoned<T>(_: T) -> Z8Error {
    Z8Error::Internal("lance registry lock poisoned".into())
}

impl Default for LanceRegistry {
    fn default() -> Self {
        Self::new(PlannerPolicy::default())
    }
}

impl LanceRegistry {
    /// An empty registry.
    pub fn new(policy: PlannerPolicy) -> Self {
        Self {
            inner: RwLock::new(Inner {
                sources: HashMap::new(),
                source_names: Vec::new(),
                plans: HashMap::new(),
                results: HashMap::new(),
                cache: HashMap::new(),
                next: 1,
            }),
            catalog: RwLock::new(Catalog::default()),
            cam: RwLock::new(CamLabels::default()),
            kv: RwLock::new(MemKv::default()),
            policy,
            stats: RegistryStats::default(),
        }
    }

    /// Publish (or republish) a batch under a name. Republishing replaces the
    /// batch and drops cached results for it; older handles fail closed on
    /// their generation.
    pub fn publish(&self, name: impl Into<String>, batch: AbiBatch) -> Z8Result<()> {
        let mut g = self.inner.write().map_err(poisoned)?;
        let id = batch.source();
        let name = name.into();
        g.source_names.retain(|(n, _)| *n != name);
        g.source_names.push((name, id));
        g.cache.retain(|k, _| k.source.id != id);
        g.sources.insert(id, Arc::new(batch));
        Ok(())
    }

    /// Resolve a published source name (node configuration boundary).
    pub fn source_id(&self, name: &str) -> Z8Result<SourceId> {
        let g = self.inner.read().map_err(poisoned)?;
        g.source_names
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
            .ok_or_else(|| Z8Error::Internal(format!("no lance source published as '{name}'")))
    }

    /// The current batch of a source.
    pub fn batch(&self, id: SourceId) -> Z8Result<Arc<AbiBatch>> {
        let g = self.inner.read().map_err(poisoned)?;
        g.sources
            .get(&id)
            .cloned()
            .ok_or_else(|| Z8Error::Internal(format!("no lance source {id}")))
    }

    fn check(&self, g: &Inner, e: &Envelope) -> Z8Result<()> {
        let cur = g
            .sources
            .get(&e.source)
            .ok_or_else(|| Z8Error::Internal(format!("no lance source {}", e.source)))?;
        if cur.generation() != e.generation {
            return Err(Z8Error::Internal(format!(
                "stale lance handle: source {} is at generation {}, handle minted at {}",
                e.source,
                cur.generation(),
                e.generation
            )));
        }
        Ok(())
    }

    /// Register a plan; returns its envelope.
    pub fn put_plan(&self, plan: ReportPlan) -> Z8Result<Envelope> {
        let mut g = self.inner.write().map_err(poisoned)?;
        let h = g.next;
        g.next += 1;
        let env = Envelope {
            role: Role::Plan,
            handle: h,
            generation: plan.source.generation,
            source: plan.source.id,
        };
        g.plans.insert(h, Arc::new(plan));
        Ok(env)
    }

    /// Resolve a plan handle.
    pub fn plan(&self, e: &Envelope) -> Z8Result<Arc<ReportPlan>> {
        let g = self.inner.read().map_err(poisoned)?;
        self.check(&g, e)?;
        g.plans
            .get(&e.handle)
            .cloned()
            .ok_or_else(|| Z8Error::Internal(format!("no lance plan handle {}", e.handle)))
    }

    /// Register a result; returns its envelope.
    pub fn put_result(&self, plan: &ReportPlan, r: ReportResult) -> Z8Result<Envelope> {
        let mut g = self.inner.write().map_err(poisoned)?;
        let h = g.next;
        g.next += 1;
        g.results.insert(h, r);
        Ok(Envelope {
            role: Role::Result,
            handle: h,
            generation: plan.source.generation,
            source: plan.source.id,
        })
    }

    /// Resolve a result handle.
    pub fn result(&self, e: &Envelope) -> Z8Result<ReportResult> {
        let g = self.inner.read().map_err(poisoned)?;
        self.check(&g, e)?;
        g.results
            .get(&e.handle)
            .cloned()
            .ok_or_else(|| Z8Error::Internal(format!("no lance result handle {}", e.handle)))
    }

    /// Execute a plan, or re-view a cached aggregate space whose physical key
    /// matches (roles, axis order and top-k are not part of the key).
    pub fn execute(&self, plan: &ReportPlan) -> Z8Result<ReportResult> {
        {
            let g = self.inner.read().map_err(poisoned)?;
            if let Some(hit) = g.cache.get(&plan.physical_key()) {
                self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
                return hit
                    .reinterpret(plan)
                    .map_err(|e| Z8Error::Internal(e.to_string()));
            }
        }
        let batch = self.batch(plan.source.id)?;
        let (res, _) = plan
            .execute(&batch, &self.policy)
            .map_err(|e| Z8Error::Internal(e.to_string()))?;
        self.stats.executions.fetch_add(1, Ordering::Relaxed);
        let mut g = self.inner.write().map_err(poisoned)?;
        g.cache.insert(plan.physical_key(), res.clone());
        Ok(res)
    }
}
