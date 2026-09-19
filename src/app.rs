//! Async application facade shared by the CLI and MCP composition roots.

use camino::{Utf8Path, Utf8PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

use crate::document::types::PathScope;
use crate::error::{AgentWikiError, Result};
use crate::governance::types::{
    Issue, RulesRequest, RulesResult, Severity, ValidationRequest, ValidationResult,
    ValidationScope,
};
use crate::projection::{Projection, types::SyncReport};
use crate::retrieval::types::{ContextQuery, SearchResult};

#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub wiki_root: Utf8PathBuf,
    pub projection_dir: Utf8PathBuf,
    pub embedding_model: Option<String>,
}

pub struct AgentWiki {
    root: Utf8PathBuf,
    projection: Mutex<Projection>,
    operation: Mutex<()>,
    blocking: Arc<Semaphore>,
}

impl AgentWiki {
    /// Return the canonical Wiki root used for protocol-facing absolute paths.
    pub fn wiki_root(&self) -> &Utf8Path {
        &self.root
    }

    pub async fn open(options: OpenOptions) -> Result<Self> {
        tokio::fs::create_dir_all(&options.wiki_root)
            .await
            .map_err(|source| AgentWikiError::Io {
                path: options.wiki_root.clone(),
                source,
            })?;
        let root = tokio::fs::canonicalize(&options.wiki_root)
            .await
            .map_err(|source| AgentWikiError::Io {
                path: options.wiki_root.clone(),
                source,
            })?;
        let root = Utf8PathBuf::from_path_buf(root)
            .map_err(|path| AgentWikiError::Config(format!("wiki root is not UTF-8: {path:?}")))?;
        seed_agentwiki(&root).await?;
        let projection = Projection::assemble_with_embedding(
            &root,
            &options.projection_dir,
            options.embedding_model.as_deref(),
        )
        .await?;
        let permits = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .clamp(1, 4);
        Ok(Self {
            root,
            projection: Mutex::new(projection),
            operation: Mutex::new(()),
            blocking: Arc::new(Semaphore::new(permits)),
        })
    }

    pub async fn sync(&self) -> Result<SyncReport> {
        let _operation = self.operation.lock().await;
        self.projection.lock().await.ensure_fresh().await
    }

    pub async fn rebuild(&self) -> Result<SyncReport> {
        let _operation = self.operation.lock().await;
        self.projection.lock().await.rebuild().await
    }

    /// Explicit maintenance for derived indices; never triggered by queries.
    pub async fn maintain_indexes(&self) -> Result<()> {
        let _operation = self.operation.lock().await;
        self.projection.lock().await.maintain().await
    }

    pub async fn query(&self, query: ContextQuery) -> Result<SearchResult> {
        validate_query(&query)?;
        crate::document::scope_path(&self.root, Utf8Path::new(&query.scope))?;
        let _operation = self.operation.lock().await;
        let mut projection = self.projection.lock().await;
        let report = projection.ensure_fresh().await?;
        let mut result = crate::retrieval::search::run_query(&projection, &query).await?;
        result.degraded.extend(report.degraded);
        Ok(result)
    }

    pub async fn validate(&self, request: ValidationRequest) -> Result<ValidationResult> {
        let _operation = self.operation.lock().await;
        let root = self.root.clone();
        self.run_blocking(move || validate_blocking(&root, request))
            .await
    }

    pub async fn rules(&self, request: RulesRequest) -> Result<RulesResult> {
        crate::document::scope_path(&self.root, Utf8Path::new(&request.scope))?;
        let _operation = self.operation.lock().await;
        let known_tags = {
            let mut projection = self.projection.lock().await;
            // Rules only need fresh literal metadata, so skip semantic inference.
            projection.ensure_fresh_without_vectors().await?;
            let mut counts: Vec<_> = projection.index.all_tags().await?.into_iter().collect();
            counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            counts.into_iter().map(|(tag, _)| tag).collect()
        };
        let root = self.root.clone();
        self.run_blocking(move || {
            let path = PathScope("AGENTWIKI.md".into());
            let (document, guide_content) =
                crate::document::parse::read_document_with_body(&root, &path)?;
            let parsed = crate::governance::rules::parse_rules(&document.frontmatter)?;
            let effective = crate::governance::rules::effective_rules(&request.scope, &parsed);
            Ok(RulesResult {
                wiki_root: root,
                guide_content,
                source_modified_at_ns: document.fingerprint.mtime_ns,
                source_size: document.fingerprint.size,
                default_type: effective.default_type,
                required_fields: effective.required_fields,
                tag_aliases: effective.tag_aliases,
                sections: effective.sections,
                known_tags,
                scope: request.scope,
            })
        })
        .await
    }

    async fn run_blocking<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = self
            .blocking
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| AgentWikiError::Other(error.to_string()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| AgentWikiError::Other(format!("blocking task failed: {error}")))?
    }
}

fn validate_query(query: &ContextQuery) -> Result<()> {
    if !(1..=20).contains(&query.document_limit)
        || !(1..=20).contains(&query.fragment_limit)
        || query.query.len() > 8192
        || query.keywords.len() > 20
        || query.keywords.iter().any(|keyword| keyword.len() > 256)
        || query.tags.len() > 100
        || query.note_types.len() > 100
        || query.metadata_filters.len() > 100
        || query
            .modified_after_ns
            .zip(query.modified_before_ns)
            .is_some_and(|(after, before)| after > before)
    {
        return Err(AgentWikiError::Config("invalid query limits".into()));
    }
    Ok(())
}

fn validate_blocking(root: &Utf8Path, request: ValidationRequest) -> Result<ValidationResult> {
    let scope = match &request.scope {
        ValidationScope::Document(path) => {
            crate::document::scope_path(root, &path.0)?;
            if path.0.as_str() == "AGENTWIKI.md" || path.0.extension() != Some("md") {
                return Err(AgentWikiError::Config(
                    "expected an ordinary Markdown document".into(),
                ));
            }
            Some(path)
        }
        ValidationScope::All => None,
    };
    let mut formatted_paths = Vec::new();
    let mut failures = Vec::new();
    if request.fix_format {
        let paths = match scope {
            Some(path) => vec![path.clone()],
            None => crate::document::snapshot(root)?,
        };
        for path in paths {
            match crate::governance::format::format_file(root, &path) {
                Ok(true) => formatted_paths.push(path.0.to_string()),
                Ok(false) => {}
                Err(AgentWikiError::FormatConflict { .. }) => failures.push(Issue {
                    path: path.0.to_string(),
                    kind: "format.conflict".into(),
                    message: "formatting conflict; file changed externally".into(),
                    severity: Severity::Error,
                }),
                Err(error) => failures.push(Issue {
                    path: path.0.to_string(),
                    kind: "format.failed".into(),
                    message: error.to_string(),
                    severity: Severity::Error,
                }),
            }
        }
    }
    let mut issues = crate::governance::validate::validate_wiki(root, scope)?;
    issues.extend(failures);
    Ok(ValidationResult {
        issues,
        formatted_paths,
    })
}

async fn seed_agentwiki(root: &Utf8Path) -> Result<()> {
    let path = root.join("AGENTWIKI.md");
    if tokio::fs::try_exists(&path)
        .await
        .map_err(|source| AgentWikiError::Io {
            path: path.clone(),
            source,
        })?
    {
        return Ok(());
    }
    tokio::fs::write(&path, crate::document::DEFAULT_AGENTWIKI)
        .await
        .map_err(|source| AgentWikiError::Io { path, source })
}
