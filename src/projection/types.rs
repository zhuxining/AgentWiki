use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub indexed: usize,
    pub removed: usize,
    pub moved: usize,
    pub unchanged: usize,
    pub vectors_ready: usize,
    pub vectors_pending: usize,
    pub degraded: Vec<String>,
    pub generation: String,
}
