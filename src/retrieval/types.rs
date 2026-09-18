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
    pub documents: Vec<RankedSlice>,
    pub fragments: Vec<RankedSlice>,
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
    pub document_limit: usize,
    pub fragment_limit: usize,
    pub keywords: Vec<String>,
    pub keyword_mode: KeywordMode,
    pub tags: Vec<String>,
    pub note_types: Vec<String>,
    pub metadata_filters: Frontmatter,
    pub modified_after_ns: Option<i64>,
    pub modified_before_ns: Option<i64>,
    pub order: SearchOrder,
    pub include_relations: bool,
    pub min_similarity: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum KeywordMode {
    #[default]
    Any,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "mcp", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SearchOrder {
    #[default]
    Relevance,
    ModifiedDesc,
}

impl Default for ContextQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            scope: String::new(),
            document_limit: 5,
            fragment_limit: 10,
            keywords: Vec::new(),
            keyword_mode: KeywordMode::Any,
            tags: Vec::new(),
            note_types: Vec::new(),
            metadata_filters: Frontmatter::new(),
            modified_after_ns: None,
            modified_before_ns: None,
            order: SearchOrder::Relevance,
            include_relations: false,
            min_similarity: SEMANTIC_MIN_SIMILARITY,
        }
    }
}

impl ContextQuery {
    pub fn is_recent_request(&self) -> bool {
        self.query.trim().is_empty() && self.keywords.is_empty()
    }
}

pub const SEMANTIC_MIN_SIMILARITY: f64 = 0.46;
