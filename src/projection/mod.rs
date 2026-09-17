//! Rebuildable LanceDB and SQLite projections.

pub(crate) mod embedding;
pub(crate) mod lance;
pub(crate) mod metadata;
pub(crate) mod sync;
pub mod types;

pub(crate) use lance::LanceIndex;
pub(crate) use sync::Projection;
