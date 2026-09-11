//! Pure domain types (a `model`, formerly "domain").
//!
//! This module is the dependency-free foundation: it must not import Tantivy,
//! SQLite, MCP, CLAP, or touch the filesystem. It holds value objects and query
//! structs that are serialised to JSON for the MCP layer and used as the
//! contract between `sync`, `search` and `validate`.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// A path already validated to live inside the wiki root.
///
/// Invariant: `value` is a relative path within the wiki root (no `..` escape,
/// no absolute prefix). Construction/verification happens in `markdown`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PathScope(pub Utf8PathBuf);

/// File-level fingerprint used by the incremental sync fast path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// SHA-256 of the file content.
    pub content_hash: String,
    /// Real mtime in nanoseconds; overrides any index write time.
    pub mtime_ns: i64,
    /// Byte size of the file.
    pub size: u64,
}

/// Frontmatter metadata extracted from the YAML header, kept as a flat map so
/// arbitrary user fields are allowed (the system never hard-codes required
/// fields).
pub type Frontmatter = serde_json::Map<String, serde_json::Value>;

/// A parsed Markdown document with its frontmatter and fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub path: PathScope,
    pub title: String,
    pub frontmatter: Frontmatter,
    pub fingerprint: Fingerprint,
}

/// A heading-aware slice of a document, produced by `markdown::chunk_document`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slice {
    pub path: PathScope,
    /// Deterministic id, e.g. SHA-256 of (path, ordinal, source).
    pub chunk_id: String,
    /// 0-based ordinal within the document.
    pub ordinal: u32,
    /// Breadcrumb of headings, joined by " / " (empty for a headerless doc).
    pub section: String,
    /// The slice body text (never empty; a contentless doc yields one empty slice).
    pub content: String,
    /// SHA-256 of the source (section + content) used to reuse vectors.
    pub source_hash: String,
}

/// A one-hop document relation declared in the Markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: PathScope,
    pub to: PathScope,
    /// e.g. "[[link]]", a relative ".md" link, or a `relations.type`.
    pub relation_type: String,
    /// Section that declared the relation.
    pub section_source: String,
    pub status: EdgeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStatus {
    /// Target document is present in the wiki.
    Resolved,
    /// Target does not exist yet; resolved later during sync.
    Unresolved,
}

/// Governance rules carried by the reserved `AGENTWIKI.md`.
///
/// The system defines **no built-in required fields** — everything follows the
/// rule file. Extra fields are allowed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rule {
    /// Path-relative rules apply within; `None` means the whole wiki.
    pub scope: Option<String>,
    pub required_fields: Vec<String>,
    /// Maps synonymous tags onto a canonical tag.
    pub tag_aliases: std::collections::BTreeMap<String, String>,
    /// Per-section extension of `required_fields`.
    pub sections: Vec<RuleSection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleSection {
    pub match_path: String,
    pub required_fields: Vec<String>,
}

/// A single ranked retrieval hit for one slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedSlice {
    pub slice: Slice,
    /// Final fused score (higher is better).
    pub score: f64,
    /// Which legs contributed, for diagnostics.
    pub sources: Vec<String>,
}

/// Related documents returned alongside a retrieval bundle (one hop).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedDocument {
    pub path: PathScope,
    pub relation_type: String,
    pub status: EdgeStatus,
}

/// Result of a `get_wiki_context` retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub slices: Vec<RankedSlice>,
    pub related: Vec<RelatedDocument>,
    /// Non-fatal diagnostics; presence marks a degraded query.
    pub degraded: Vec<String>,
}

/// Query parameters for a context retrieval.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContextQuery {
    /// Free-text query; empty means "recent documents".
    pub query: String,
    /// Path scope filter (a directory under the wiki root).
    pub scope: String,
    /// Maximum number of ranked slices to return.
    pub limit: usize,
    /// Tag filter (all must be present).
    pub tags: Vec<String>,
    /// Note-type / frontmatter filter.
    pub note_types: Vec<String>,
    /// Prefix key match on frontmatter.
    pub metadata_filters: Frontmatter,
    /// Minimum vector similarity to consider a semantic hit.
    pub min_similarity: f64,
}

/// Report of one sync / rebuild round.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub indexed: usize,
    pub removed: usize,
    pub moved: usize,
    pub unchanged: usize,
    pub vectors_ready: usize,
    pub vectors_pending: usize,
    /// Non-fatal per-document failures.
    pub degraded: Vec<String>,
    /// Generation marker for the fast path.
    pub generation: String,
}

impl ContextQuery {
    /// True when this query carries only filters and no free text.
    pub fn is_recent_request(&self) -> bool {
        self.query.trim().is_empty()
    }
}
