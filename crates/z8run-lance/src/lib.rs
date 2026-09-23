//! # z8run-lance — z8run nodes over the lance-graph reporting substrate
//!
//! z8run owns orchestration: the visual DAG, triggers, scheduling, and the
//! construction of a report plan node by node. lance-graph owns everything
//! physical: lanes, masks, folds, pivots. This crate is the seam, and the
//! seam carries ONE thing — a handle:
//!
//! ```json
//! {"$kind":"lance-abi","role":"plan","handle":17,"generation":4,"source":9}
//! ```
//!
//! **JSON is control plane, not data plane.** No node here ever puts a row,
//! a lane, a mask or an aggregate cell into a `FlowMessage`. A plan node
//! rewrites an immutable `Arc<ReportPlan>` and emits a new handle; the
//! execute node folds in lance-graph and emits a result handle; only the
//! `lance-materialize` node — the explicit terminal boundary — turns a result
//! into text (JSON / CSV / HTML), resolving labels through CAM for the
//! presented members only.
//!
//! **Never transpose data to pivot. Transpose meaning.** `lance-pivot` on a
//! plan handle rewrites roles; on a result handle it re-views the SAME
//! aggregate space (no fold, no copy). `lance-execute` consults a cache keyed
//! by the role-free physical key, so a rotated plan reuses the fold its
//! un-rotated twin already paid for.
//!
//! **Strings stop at the boundary.** Node config is human-facing (field
//! names, labels). Names and labels are resolved to `FieldId`s and ordinals
//! when the node is CONFIGURED, through the registry's catalog and CAM; the
//! plans the nodes emit hold ids only.
//!
//! Fan-out is cheap by construction: sibling branches clone a JSON envelope
//! and share `Arc`s; nothing mutable is shared — every execution owns its
//! scratch (lance-graph sizes it per program).

pub mod nodes;
pub mod registry;

pub use nodes::register_lance_nodes;
pub use registry::{Envelope, LanceRegistry, RegistryStats, Role};
