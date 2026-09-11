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
/// No built-in required fields — everything follows the rule file. Unknown
/// fields are rejected (`deny_unknown_fields`) while document frontmatter
/// stays freely extensible. Contract defaults per RULES.md:
/// `version=1`, `name="AgentWiki"`, `default_type="note"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Rule schema version; must be >= 1 (enforced by `rules::parse_rules`).
    #[serde(default = "default_version")]
    pub version: i32,
    /// Wiki display name.
    #[serde(default = "default_name")]
    pub name: String,
    /// Wiki purpose, returned to agents as organisational background.
    #[serde(default)]
    pub purpose: String,
    /// Default type used when a document declares no `type`.
    #[serde(default = "default_doc_type")]
    pub default_type: String,
    /// Path-relative rules apply within; `None` means the whole wiki.
    #[serde(default)]
    pub scope: Option<String>,
    /// Root-level required fields.
    #[serde(default)]
    pub required_fields: Vec<String>,
    /// Maps a canonical tag onto its alias list, for tag normalisation.
    #[serde(default)]
    pub tag_aliases: std::collections::BTreeMap<String, Vec<String>>,
    /// Per-directory/per-pattern extension of `required_fields` and types.
    #[serde(default)]
    pub sections: Vec<RuleSection>,
}

fn default_version() -> i32 {
    1
}

fn default_name() -> String {
    "AgentWiki".to_string()
}

fn default_doc_type() -> String {
    "note".to_string()
}

/// One directory rule: `path` matches a wiki-relative directory (and all its
/// descendants) or an fnmatch pattern whose `*` matches `/`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSection {
    /// Directory or fnmatch pattern, e.g. `guides`, `projects/*`.
    pub path: String,
    /// Human-readable purpose of this scope.
    #[serde(default)]
    pub description: String,
    /// Allowed document types within this scope (empty = unrestricted).
    #[serde(default)]
    pub types: Vec<String>,
    /// Extra required fields appended for matching documents.
    #[serde(default)]
    pub required_fields: Vec<String>,
    /// File-name fnmatch pattern; a mismatch is a `path.filename` issue.
    #[serde(default)]
    pub filename_pattern: Option<String>,
}

/// Root rules merged with every section matching one wiki-relative path.
///
/// Shared contract between `validate` (today) and the MCP `get_wiki_rules`
/// tool (later); built by `rules::effective_rules`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectiveRules {
    /// Root `default_type` (or contract default `note`).
    pub default_type: String,
    /// Root plus every matching section's required fields, deduplicated with
    /// more specific sections applied last.
    pub required_fields: Vec<String>,
    /// Root `tag_aliases`, canonical tag -> alias list.
    pub tag_aliases: std::collections::BTreeMap<String, Vec<String>>,
    /// Matching sections in ascending `path` length (most specific last).
    pub sections: Vec<RuleSection>,
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

/// Severity of a validation finding (`validate` output contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// A single validation finding produced by `validate::validate_wiki`.
/// `kind` is a stable machine string, `severity` error vs advisory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// Wiki-relative document path.
    pub path: String,
    /// Stable machine kind, e.g. `frontmatter.required`, `link.broken`, `markdown.parse`.
    pub kind: String,
    /// Human-readable explanation.
    pub message: String,
    /// `Error` blocks conformance; `Warning` is advisory (e.g. tag suggestions).
    pub severity: Severity,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
