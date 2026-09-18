//! Incremental projection; confirm the ledger only after successful writes.
use crate::document::types::{Fingerprint, PathScope};
use crate::error::{AgentWikiError, Result};
use crate::projection::types::SyncReport;
use crate::{
    document,
    document::relation,
    projection::{
        LanceIndex,
        embedding::Embedder,
        metadata::{LedgerRow, Metadata},
    },
};
use camino::Utf8Path;
use sha2::{Digest, Sha256};

pub struct Projection {
    pub root: camino::Utf8PathBuf,
    pub index: LanceIndex,
    pub meta: Metadata,
    pub embedder: Option<std::sync::Arc<std::sync::Mutex<Embedder>>>,
    pub embedding_error: Option<String>,
    lock_path: camino::Utf8PathBuf,
}

impl Projection {
    pub async fn assemble_with_embedding(
        root: &Utf8Path,
        index_dir: &Utf8Path,
        model: Option<&str>,
    ) -> Result<Self> {
        if model.is_some_and(|m| m != "BAAI/bge-small-zh-v1.5") {
            return Err(AgentWikiError::Config("unsupported embedding model".into()));
        }
        tokio::fs::create_dir_all(index_dir)
            .await
            .map_err(|source| AgentWikiError::Io {
                path: index_dir.into(),
                source,
            })?;
        let lock_path = index_dir.join("sync.lock");
        let lock_target = lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_target))
            .await
            .map_err(|error| AgentWikiError::Other(format!("lock task failed: {error}")))??;
        let initialized = match model {
            Some(_) => Some(
                tokio::task::spawn_blocking(Embedder::bge_small_zh)
                    .await
                    .map_err(|error| {
                        AgentWikiError::Other(format!("embedding task failed: {error}"))
                    })?,
            ),
            None => None,
        };
        let (embedder, embedding_error) = match initialized {
            Some(Ok(embedder)) => (
                Some(std::sync::Arc::new(std::sync::Mutex::new(embedder))),
                None,
            ),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        let meta = Metadata::open(index_dir.to_path_buf()).await?;
        if meta.with(|store| Ok(store.needs_rebuild)).await? {
            // Projection format mismatch (schema or retrieval format version):
            // wipe the LanceDB library and the ledger so the fresh `LanceIndex`
            // below is recreated with current parameters (FTS tokenizer config
            // lives inside the index — deleting rows would keep the old one).
            let stale = index_dir.join("lancedb");
            tokio::task::spawn_blocking(move || remove_all(stale))
                .await
                .map_err(|error| {
                    AgentWikiError::Other(format!("cleanup task failed: {error}"))
                })??;
            meta.with(|store| store.clear()).await?;
        }
        let index = LanceIndex::open(index_dir, model.map(|_| 512)).await?;
        Ok(Self {
            root: root.into(),
            index,
            meta,
            embedder,
            embedding_error,
            lock_path,
        })
    }
    pub async fn ensure_fresh(&mut self) -> Result<SyncReport> {
        let lock_path = self.lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_path))
            .await
            .map_err(|error| AgentWikiError::Other(format!("lock task failed: {error}")))??;
        self.sync_locked().await
    }
    async fn sync_locked(&mut self) -> Result<SyncReport> {
        // Enumerate before reading: a failed read must never imply deletion.
        let root = self.root.clone();
        let paths = tokio::task::spawn_blocking(move || document::snapshot(&root))
            .await
            .map_err(|error| AgentWikiError::Other(format!("scan task failed: {error}")))??;
        let current: std::collections::BTreeSet<_> =
            paths.iter().map(|p| p.0.to_string()).collect();
        let previous = self.meta.with(|store| store.ledger_snapshot()).await?;
        // Removed paths grouped by their last known content hash. A unique
        // deleted/new hash pair is reported as a move; paths remain identity.
        let mut removed_by_hash: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for (path, fp) in &previous {
            if !current.contains(path.as_str()) {
                removed_by_hash
                    .entry(fp.content_hash.clone())
                    .or_default()
                    .push(path.clone());
            }
        }
        let mut report = SyncReport::default();
        if let Some(error) = &self.embedding_error {
            report
                .degraded
                .push(format!("embedding unavailable: {error}"));
        }
        for path in &paths {
            let outcome: Result<bool> = async {
                document::scope_path(&self.root, &path.0)?;
                let full = self.root.join(&path.0);
                let metadata =
                    tokio::fs::metadata(&full)
                        .await
                        .map_err(|source| AgentWikiError::Io {
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
                let path_text = path.0.to_string();
                let embedding_hash = self
                    .meta
                    .with(move |store| store.vector_hash(&path_text))
                    .await?;
                let vectors_current = self.embedder.is_none() || embedding_hash.is_some();
                if vectors_current
                    && prev.is_some_and(|p| p.mtime_ns == stamp.mtime_ns && p.size == stamp.size)
                {
                    return Ok(false);
                }
                let read_root = self.root.clone();
                let read_path = path.clone();
                let (doc, body) = tokio::task::spawn_blocking(move || {
                    document::parse::read_document_with_body(&read_root, &read_path)
                })
                .await
                .map_err(|error| {
                    AgentWikiError::Other(format!("document task failed: {error}"))
                })??;
                if vectors_current
                    && prev.is_some_and(|p| p.content_hash == doc.fingerprint.content_hash)
                {
                    let path_text = path.0.to_string();
                    let fingerprint = doc.fingerprint.clone();
                    self.meta
                        .with(move |store| store.update_fingerprint(&path_text, &fingerprint))
                        .await?;
                    return Ok(false);
                }
                let slices = document::chunk_document(
                    &doc.frontmatter,
                    &body,
                    path,
                    doc.fingerprint.mtime_ns,
                );
                let (edges, warnings) = relation::extract_edges(path, &doc.frontmatter, &body);
                report
                    .degraded
                    .extend(warnings.into_iter().map(|w| format!("{}: {w}", path.0)));
                self.index.replace_slices(path, &slices).await?;
                self.index.replace_relations(path, &edges).await?;
                if prev.is_none()
                    && removed_by_hash
                        .get(&doc.fingerprint.content_hash)
                        .is_some_and(|candidates| candidates.len() == 1)
                {
                    report.moved += 1;
                }
                let row = LedgerRow {
                    path: path.0.to_string(),
                    content_hash: doc.fingerprint.content_hash.clone(),
                    mtime_ns: doc.fingerprint.mtime_ns,
                    size: doc.fingerprint.size,
                    vector_input_hash: None,
                };
                self.meta
                    .with(move |store| store.upsert_ledger(&row))
                    .await?;
                if let Some(embedder) = &self.embedder {
                    let inputs: Vec<String> = slices
                        .iter()
                        .map(|slice| slice.search_text.clone())
                        .collect();
                    let input_hash = hex::encode(Sha256::digest(format!(
                        "BAAI/bge-small-zh-v1.5:512:search-text-v2:{inputs:?}"
                    )));
                    if embedding_hash.as_deref() == Some(&input_hash) {
                        let path_text = path.0.to_string();
                        let confirmed_hash = input_hash.clone();
                        self.meta
                            .with(move |store| store.confirm_vectors(&path_text, &confirmed_hash))
                            .await?;
                        return Ok(true);
                    }
                    let embedder = embedder.clone();
                    let vectors = tokio::task::spawn_blocking(move || {
                        let mut embedder = embedder
                            .lock()
                            .map_err(|error| AgentWikiError::Embedding(error.to_string()))?;
                        embedder.embed(inputs).map_err(AgentWikiError::Embedding)
                    })
                    .await
                    .map_err(|error| {
                        AgentWikiError::Other(format!("embedding task failed: {error}"))
                    })??;
                    self.index.replace_vectors(path, &slices, &vectors).await?;
                    report.vectors_ready += vectors.len();
                    let path_text = path.0.to_string();
                    self.meta
                        .with(move |store| store.confirm_vectors(&path_text, &input_hash))
                        .await?;
                }
                Ok(true)
            }
            .await;
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
            let deletion = match self.index.delete_path(&scope).await {
                Ok(()) => {
                    let path = scope.0.to_string();
                    self.meta
                        .with(move |store| store.delete_ledger(&path))
                        .await
                }
                Err(error) => Err(error),
            };
            match deletion {
                Ok(()) => report.removed += 1,
                Err(error) => report
                    .degraded
                    .push(format!("{path}: deletion failed: {error}")),
            }
        }
        report.generation = hex::encode(Sha256::digest(format!(
            "{:?}",
            self.meta.with(|store| store.ledger_snapshot()).await?
        )));
        // Confirm the retrieval format only after the projection round
        // succeeded, keeping a failed round force a full rebuild next time.
        self.meta
            .with(|store| store.set_retrieval_format(crate::projection::metadata::RETRIEVAL_FORMAT))
            .await?;
        Ok(report)
    }
    pub async fn rebuild(&mut self) -> Result<SyncReport> {
        let lock_path = self.lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_path))
            .await
            .map_err(|error| AgentWikiError::Other(format!("lock task failed: {error}")))??;
        self.index.reset().await?;
        self.meta.with(|store| store.clear()).await?;
        self.sync_locked().await
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

/// Recursively remove a directory, treating a missing path as success.
fn remove_all(path: camino::Utf8PathBuf) -> Result<()> {
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AgentWikiError::Io { path, source }),
    }
}
