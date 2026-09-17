use serde::{Deserialize, Serialize};

use crate::document::types::PathScope;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default = "default_version")]
    pub version: i32,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default = "default_doc_type")]
    pub default_type: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default)]
    pub tag_aliases: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub sections: Vec<RuleSection>,
}

fn default_version() -> i32 {
    1
}
fn default_name() -> String {
    "AgentWiki".to_owned()
}
fn default_doc_type() -> String {
    "note".to_owned()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSection {
    pub path: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default)]
    pub filename_pattern: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectiveRules {
    pub default_type: String,
    pub required_fields: Vec<String>,
    pub tag_aliases: std::collections::BTreeMap<String, Vec<String>>,
    pub sections: Vec<RuleSection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub path: String,
    pub kind: String,
    pub message: String,
    pub severity: Severity,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, Clone)]
pub enum ValidationScope {
    Document(PathScope),
    All,
}

#[derive(Debug, Clone)]
pub struct ValidationRequest {
    pub scope: ValidationScope,
    pub fix_format: bool,
}

#[derive(Debug, Serialize)]
pub struct ValidationResult {
    pub issues: Vec<Issue>,
    pub formatted_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RulesRequest {
    pub scope: String,
}

#[derive(Debug, Serialize)]
pub struct RulesResult {
    pub wiki_root: camino::Utf8PathBuf,
    pub guide_content: String,
    pub source_modified_at_ns: i64,
    pub source_size: u64,
    pub default_type: String,
    pub required_fields: Vec<String>,
    pub tag_aliases: std::collections::BTreeMap<String, Vec<String>>,
    pub sections: Vec<RuleSection>,
    pub known_tags: Vec<String>,
    pub scope: String,
}
