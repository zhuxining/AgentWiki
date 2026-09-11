//! Incremental projection; confirm the ledger only after successful writes.
use crate::error::{AgentWikiError, Result};
use crate::model::{Fingerprint, PathScope, SyncReport};
use crate::{
    document, graph,
    retrieval::{
        LanceIndex,
        embedding::{Embedder, input_text},
    },
    storage::MetaStore,
};
use camino::Utf8Path;
use sha2::{Digest, Sha256};

pub struct SyncContext {
    pub root: camino::Utf8PathBuf,
    pub index: LanceIndex,
    pub meta: MetaStore,
    pub embedder: Option<std::sync::Mutex<Embedder>>,
    pub embedding_error: Option<String>,
    lock_path: camino::Utf8PathBuf,
}

impl SyncContext {
    pub fn assemble(root: &Utf8Path, index_dir: &Utf8Path) -> Result<Self> {
        Self::assemble_with_embedding(root, index_dir, None)
    }
    pub fn assemble_with_embedding(
        root: &Utf8Path,
        index_dir: &Utf8Path,
        model: Option<&str>,
    ) -> Result<Self> {
        if model.is_some_and(|m| m != "BAAI/bge-small-zh-v1.5") {
            return Err(AgentWikiError::Config("unsupported embedding model".into()));
        }
        std::fs::create_dir_all(index_dir).map_err(|source| AgentWikiError::Io {
            path: index_dir.into(),
            source,
        })?;
        let lock_path = index_dir.join("sync.lock");
        let _lock = acquire_lock(&lock_path)?;
        let (embedder, embedding_error) = match model.map(|_| Embedder::bge_small_zh()) {
            Some(Ok(embedder)) => (Some(std::sync::Mutex::new(embedder)), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        let meta = MetaStore::open(index_dir)?;
        let index = LanceIndex::open(index_dir, model.map(|_| 512))?;
        if meta.needs_rebuild {
            index.reset()?;
        }
        Ok(Self {
            root: root.into(),
            index,
            meta,
            embedder,
            embedding_error,
            lock_path,
        })
    }
    pub fn ensure_fresh(&mut self) -> Result<SyncReport> {
        let _lock = acquire_lock(&self.lock_path)?;
        self.sync_locked()
    }
    fn sync_locked(&mut self) -> Result<SyncReport> {
        // Enumerate before reading: a failed read must never imply deletion.
        let paths = document::snapshot(&self.root)?;
        let current: std::collections::BTreeSet<_> =
            paths.iter().map(|p| p.0.to_string()).collect();
        let previous = self.meta.ledger_snapshot()?;
        let mut report = SyncReport::default();
        if let Some(error) = &self.embedding_error {
            report
                .degraded
                .push(format!("embedding unavailable: {error}"));
        }
        for path in &paths {
            let outcome = (|| -> Result<bool> {
                document::scope_path(&self.root, &path.0)?;
                let full = self.root.join(&path.0);
                let metadata = std::fs::metadata(&full).map_err(|source| AgentWikiError::Io {
                    path: full.clone(),
                    source,
                })?;
                let modified = metadata
                    .modified()
                    .map_err(|source| AgentWikiError::Io { path: full, source })?;
                let stamp = Fingerprint {
                    content_hash: String::new(),
                    size: metadata.len(),
                    mtime_ns: modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as i64,
                };
                let prev = previous.get(path.0.as_str());
                let (sync_error, embedding_hash, identity) =
                    self.meta.sync_state(path.0.as_str())?;
                let vectors_current = self.embedder.is_none() || embedding_hash.is_some();
                if sync_error.is_none()
                    && vectors_current
                    && prev.is_some_and(|p| p.mtime_ns == stamp.mtime_ns && p.size == stamp.size)
                {
                    return Ok(false);
                }
                let (doc, body) = document::parse::read_document_with_body(&self.root, path)?;
                if sync_error.is_none()
                    && vectors_current
                    && prev.is_some_and(|p| p.content_hash == doc.fingerprint.content_hash)
                {
                    self.meta
                        .update_fingerprint(path.0.as_str(), &doc.fingerprint)?;
                    return Ok(false);
                }
                let slices = document::chunk_document(&doc.title, &body, path);
                let (edges, warnings) = graph::extract_edges(path, &doc.frontmatter, &body);
                report
                    .degraded
                    .extend(warnings.into_iter().map(|w| format!("{}: {w}", path.0)));
                self.index.replace_slices(path, &slices)?;
                self.meta.replace_document(path, &edges)?;
                self.meta.set_title(path, &doc.title)?;
                self.meta.upsert_ledger(&crate::storage::LedgerRow {
                    path: path.0.to_string(),
                    content_hash: doc.fingerprint.content_hash.clone(),
                    mtime_ns: doc.fingerprint.mtime_ns,
                    size: doc.fingerprint.size,
                    stable_identity: if identity.is_empty() {
                        hex::encode(Sha256::digest(format!(
                            "{}:{}",
                            path.0, doc.fingerprint.content_hash
                        )))
                    } else {
                        identity
                    },
                    sync_error: self
                        .embedder
                        .as_ref()
                        .map(|_| "vector projection incomplete".into()),
                    embedding_hash: None,
                    frontmatter_json: serde_json::to_string(&doc.frontmatter)
                        .map_err(|e| AgentWikiError::Other(e.to_string()))?,
                })?;
                if let Some(embedder) = &self.embedder {
                    let tags = doc
                        .frontmatter
                        .get("tags")
                        .and_then(|v| v.as_array())
                        .map(|v| {
                            v.iter()
                                .filter_map(|t| t.as_str().map(str::to_owned))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let inputs: Vec<String> = slices
                        .iter()
                        .map(|s| input_text(&doc.title, &tags, &s.section, &s.content))
                        .collect();
                    let input_hash = hex::encode(Sha256::digest(format!(
                        "BAAI/bge-small-zh-v1.5:512:v1:{inputs:?}"
                    )));
                    if embedding_hash.as_deref() == Some(&input_hash) {
                        self.meta.confirm_vectors(path.0.as_str(), &input_hash)?;
                        return Ok(true);
                    }
                    let vectors = embedder
                        .lock()
                        .map_err(|e| AgentWikiError::Embedding(e.to_string()))?
                        .embed(inputs)
                        .map_err(AgentWikiError::Embedding)?;
                    self.index.replace_vectors(path, &slices, &vectors)?;
                    report.vectors_ready += vectors.len();
                    self.meta.confirm_vectors(path.0.as_str(), &input_hash)?;
                }
                Ok(true)
            })();
            match outcome {
                Ok(true) => report.indexed += 1,
                Ok(false) => report.unchanged += 1,
                Err(error) => report.degraded.push(format!(
                    "{}: {error}; projection may be stale, retry on next sync",
                    path.0
                )),
            }
        }
        for path in previous.keys().filter(|p| !current.contains(*p)) {
            let scope = PathScope(path.into());
            match self
                .index
                .delete_path(&scope)
                .and_then(|()| self.meta.delete_ledger(&scope))
            {
                Ok(()) => report.removed += 1,
                Err(error) => report
                    .degraded
                    .push(format!("{path}: deletion failed: {error}")),
            }
        }
        self.meta.resolve_edges()?;
        report.generation = hex::encode(Sha256::digest(format!(
            "{:?}",
            self.meta.ledger_snapshot()?
        )));
        self.meta.set_generation(&report.generation)?;
        Ok(report)
    }
    pub fn rebuild(&mut self) -> Result<SyncReport> {
        let _lock = acquire_lock(&self.lock_path)?;
        self.index.reset()?;
        self.meta.clear()?;
        self.sync_locked()
    }
}

fn acquire_lock(path: &Utf8Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| AgentWikiError::Io {
            path: path.into(),
            source,
        })?;
    fs2::FileExt::lock_exclusive(&file).map_err(|source| AgentWikiError::Io {
        path: path.into(),
        source,
    })?;
    Ok(file)
}
