use serde::{Deserialize, Serialize};

use crate::document::types::{EdgeStatus, Frontmatter, PathScope, Slice};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedSlice {
    pub slice: Slice,
    pub score: f64,
    pub sources: Vec<String>,
    pub filename: String,
    pub modified_at_ns: i64,
    pub frontmatter: Frontmatter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedDocument {
    pub path: PathScope,
    pub filename: String,
    pub relation_type: String,
    pub direction: RelationDirection,
    #[serde(rename = "resolution_status")]
    pub status: EdgeStatus,
    #[serde(rename = "source_section")]
    pub section_source: String,
    pub context: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub strategy: SearchStrategy,
    pub slices: Vec<RankedSlice>,
    pub related: Vec<RelatedDocument>,
    pub degraded: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchStrategy {
    Recent,
    Keyword,
    Hybrid,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextQuery {
    pub query: String,
    pub scope: String,
    pub limit: usize,
    pub tags: Vec<String>,
    pub note_types: Vec<String>,
    pub metadata_filters: Frontmatter,
    pub min_similarity: f64,
}

impl Default for ContextQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            scope: String::new(),
            limit: 10,
            tags: Vec::new(),
            note_types: Vec::new(),
            metadata_filters: Frontmatter::new(),
            min_similarity: SEMANTIC_MIN_SIMILARITY,
        }
    }
}

impl ContextQuery {
    pub fn is_recent_request(&self) -> bool {
        self.query.trim().is_empty()
    }
}

pub const SEMANTIC_MIN_SIMILARITY: f64 = 0.46;
