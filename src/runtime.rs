//! Explicit resource assembly, index lifecycle and the application facade.
//!
//! `Runtime` is the composition root on which both the CLI (`cli.rs`) and the
//! MCP server (`mcp.rs`) sit. It owns the wiki root plus a [`SyncContext`], and
//! exposes the three operations the entry points need: sync, rebuild, and query
//! (also validate). Generally library errors surface as [`crate::error::AgentWikiError`]
//! and are converted to `anyhow` at the boundary.

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::Result;
use crate::model::{ContextQuery, SearchResult, SyncReport};
use crate::sync::SyncContext;

/// A fully assembled, ready-to-run wiki handle.
pub struct Runtime {
    /// Wiki root (absolute). Markdown here is the source of truth.
    pub root: Utf8PathBuf,
    /// Directory holding the LanceDB projection and the SQLite metadata store.
    pub index_dir: Utf8PathBuf,
    sync: SyncContext,
}

impl Runtime {
    pub fn validate_with_format(
        &self,
        path: Option<&crate::model::PathScope>,
        full: bool,
        fix_format: bool,
    ) -> Result<crate::model::ValidationResult> {
        if path.is_some() == full {
            return Err(crate::error::AgentWikiError::Config(
                "specify exactly one of path or full".into(),
            ));
        }
        if let Some(path) = path {
            crate::document::scope_path(&self.root, &path.0)?;
            if path.0.as_str() == "AGENTWIKI.md" || path.0.extension() != Some("md") {
                return Err(crate::error::AgentWikiError::Config(
                    "expected an ordinary Markdown document".into(),
                ));
            }
        }
        let mut formatted_paths = Vec::new();
        let mut failures = Vec::new();
        if fix_format {
            let paths = match path {
                Some(path) => vec![path.clone()],
                None => crate::document::snapshot(&self.root)?,
            };
            for path in paths {
                match crate::governance::format::format_file(&self.root, &path) {
                    Ok(true) => formatted_paths.push(path.0.to_string()),
                    Ok(false) => {}
                    Err(error) => failures.push(crate::model::Issue {
                        path: path.0.to_string(),
                        kind: "format.write".into(),
                        message: error.to_string(),
                        severity: crate::model::Severity::Error,
                    }),
                }
            }
        }
        let mut issues = self.validate(path)?;
        issues.extend(failures);
        Ok(crate::model::ValidationResult {
            issues,
            formatted_paths,
        })
    }
    /// Assemble a runtime from a wiki root and an index directory.
    ///
    /// Creates the index directory if needed, opens (or creates) the metadata
    /// store and the search index, and prepares an AGENTWIKI.md default rule
    /// seed when the wiki has none (never overwrites an existing one).
    pub fn assemble(root: &Utf8Path, index_dir: &Utf8Path) -> Result<Self> {
        Self::assemble_with_embedding(root, index_dir, None)
    }

    pub fn assemble_with_embedding(
        root: &Utf8Path,
        index_dir: &Utf8Path,
        model: Option<&str>,
    ) -> Result<Self> {
        std::fs::create_dir_all(root).map_err(|source| crate::error::AgentWikiError::Io {
            path: root.into(),
            source,
        })?;
        let root = root
            .canonicalize_utf8()
            .map_err(|source| crate::error::AgentWikiError::Io {
                path: root.into(),
                source,
            })?;
        let index_dir = index_dir.to_path_buf();
        seed_agentwiki(&root)?;
        let sync = SyncContext::assemble_with_embedding(&root, &index_dir, model)?;
        Ok(Runtime {
            root,
            index_dir,
            sync,
        })
    }

    /// Incrementally reconcile the archive with the derived projections.
    pub fn ensure_fresh(&mut self) -> Result<SyncReport> {
        self.sync.ensure_fresh()
    }

    /// Wipe the derived projections and rebuild them from Markdown.
    pub fn rebuild(&mut self) -> Result<SyncReport> {
        self.sync.rebuild()
    }

    /// Run a context retrieval (keyword + optional semantic, with ranking).
    ///
    /// Keyword may be empty to request the most recent documents.
    pub fn query(&mut self, q: &ContextQuery) -> Result<SearchResult> {
        if !(1..=20).contains(&q.limit)
            || q.query.len() > 8192
            || q.tags.len() > 100
            || q.note_types.len() > 100
            || q.metadata_filters.len() > 100
        {
            return Err(crate::error::AgentWikiError::Config(
                "invalid query limits".into(),
            ));
        }
        crate::document::scope_path(&self.root, Utf8Path::new(&q.scope))?;
        let report = self.ensure_fresh()?;
        let mut result = crate::search::run_query(&self.sync, q)?;
        result.degraded.extend(report.degraded);
        Ok(result)
    }

    /// Validate one document or the whole wiki. Reports only, never rewrites.
    pub fn validate(
        &self,
        scope: Option<&crate::model::PathScope>,
    ) -> Result<Vec<crate::model::Issue>> {
        if let Some(scope) = scope {
            crate::document::scope_path(&self.root, &scope.0)?;
        }
        crate::governance::validate::validate_wiki(&self.root, scope)
    }
}

/// Ensure `root/AGENTWIKI.md` exists, seeding a default rule file when absent.
///
/// Never overwrites an existing file. This is the only startup write that does
/// not participate in index transactions.
fn seed_agentwiki(root: &Utf8Path) -> Result<()> {
    let path = root.join("AGENTWIKI.md");
    if path.exists() {
        return Ok(());
    }
    let template = crate::document::DEFAULT_AGENTWIKI;
    std::fs::write(&path, template)
        .map_err(|e| crate::error::AgentWikiError::Io { path, source: e })
}
