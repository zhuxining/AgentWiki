use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PathScope(pub Utf8PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Fingerprint {
    pub content_hash: String,
    pub mtime_ns: i64,
    pub size: u64,
}

pub type Frontmatter = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub path: PathScope,
    pub frontmatter: Frontmatter,
    pub fingerprint: Fingerprint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slice {
    pub path: PathScope,
    pub chunk_id: String,
    pub unit_kind: RetrievalUnitKind,
    pub note_type: String,
    pub tags: Vec<String>,
    pub facets: Vec<String>,
    pub frontmatter: Frontmatter,
    pub modified_at_ns: i64,
    pub ordinal: u32,
    pub section: String,
    pub content: String,
    pub search_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalUnitKind {
    Document,
    Fragment,
}

impl RetrievalUnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Fragment => "fragment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: PathScope,
    pub to: PathScope,
    pub relation_type: String,
    pub section_source: String,
    pub status: EdgeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStatus {
    Resolved,
    Unresolved,
}
