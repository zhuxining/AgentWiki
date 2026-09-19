//! Incremental Markdown -> LanceDB projection.

use crate::document::parse::{DocumentRead, read_document_if_changed};
use crate::document::types::{Fingerprint, PathScope, Slice};
use crate::error::{AgentWikiError, Result};
use crate::projection::types::SyncReport;
use crate::{
    document,
    document::relation,
    projection::{LanceIndex, embedding::Embedder, lance::embedding_input_hash},
};
use camino::Utf8Path;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

const EMBEDDING_IDENTITY: &str = "BAAI/bge-small-zh-v1.5";
/// Bounded local inference batch; keeps peak memory predictable on large imports.
const EMBED_BATCH_SIZE: usize = 32;

pub struct Projection {
    pub root: camino::Utf8PathBuf,
    pub index: LanceIndex,
    /// Configured model identity, or `None` when semantic search is disabled.
    pub embedding_model: Option<&'static str>,
    embedder: OnceLock<Arc<Mutex<Embedder>>>,
    embedding_error: Mutex<Option<String>>,
    lock_path: camino::Utf8PathBuf,
}

impl Projection {
    pub async fn assemble_with_embedding(
        root: &Utf8Path,
        index_dir: &Utf8Path,
        model: Option<&str>,
    ) -> Result<Self> {
        if model.is_some_and(|m| m != EMBEDDING_IDENTITY) {
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
            .map_err(|e| AgentWikiError::Other(format!("lock task failed: {e}")))??;
        let index = LanceIndex::open(index_dir, model.map(|_| 512)).await?;
        Ok(Self {
            root: root.into(),
            index,
            embedding_model: model.map(|_| EMBEDDING_IDENTITY),
            embedder: OnceLock::new(),
            embedding_error: Mutex::new(None),
            lock_path,
        })
    }

    /// Load the local model on first use so unrelated use cases never pay for it.
    /// A load failure is reported as degradation and retried on the next call.
    pub(crate) async fn embedder(&self) -> Option<Arc<Mutex<Embedder>>> {
        self.embedding_model?;
        if let Some(embedder) = self.embedder.get() {
            return Some(embedder.clone());
        }
        let loaded = tokio::task::spawn_blocking(Embedder::bge_small_zh)
            .await
            .map_err(|e| format!("embedding task failed: {e}"))
            .and_then(|result| result);
        match loaded {
            Ok(embedder) => {
                let shared = Arc::new(Mutex::new(embedder));
                // Query and sync are serialized by the caller's locks, so a racing
                // loser only discards one redundant instance.
                let _ = self.embedder.set(shared);
                self.embedder.get().cloned()
            }
            Err(error) => {
                if let Ok(mut current) = self.embedding_error.lock() {
                    *current = Some(error);
                }
                None
            }
        }
    }

    fn embedding_degraded(&self, report: &mut SyncReport) {
        if let Ok(current) = self.embedding_error.lock()
            && let Some(error) = current.as_ref()
        {
            report
                .degraded
                .push(format!("embedding unavailable: {error}"));
        }
    }

    pub async fn ensure_fresh(&mut self) -> Result<SyncReport> {
        self.sync_with_vectors(false, true).await
    }

    /// Refresh document metadata without paying for semantic inference. Rule
    /// lookups need a fresh tag set, not vectors, so they use this path.
    pub async fn ensure_fresh_without_vectors(&mut self) -> Result<SyncReport> {
        self.sync_with_vectors(false, false).await
    }

    async fn sync_with_vectors(&mut self, rebuild: bool, embed: bool) -> Result<SyncReport> {
        let lock_path = self.lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_path))
            .await
            .map_err(|e| AgentWikiError::Other(format!("lock task failed: {e}")))??;
        self.sync_locked(rebuild, embed).await
    }

    async fn sync_locked(&mut self, rebuild: bool, embed: bool) -> Result<SyncReport> {
        let root = self.root.clone();
        let paths = tokio::task::spawn_blocking(move || document::snapshot(&root))
            .await
            .map_err(|e| AgentWikiError::Other(format!("scan task failed: {e}")))??;
        let current: BTreeSet<String> = paths.iter().map(|p| p.0.to_string()).collect();
        let previous = self.index.document_fingerprints().await?;
        let embedding_on = embed && self.embedding_model.is_some();
        let missing_vectors = if embedding_on {
            self.index.documents_missing_vectors().await?
        } else {
            Default::default()
        };
        let mut removed_by_hash: HashMap<String, Vec<String>> = HashMap::new();
        for (path, fp) in &previous {
            if !current.contains(path) {
                removed_by_hash
                    .entry(fp.content_hash.clone())
                    .or_default()
                    .push(path.clone());
            }
        }
        let mut report = SyncReport::default();
        self.embedding_degraded(&mut report);
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
                let modified = metadata.modified().map_err(|source| AgentWikiError::Io {
                    path: full.clone(),
                    source,
                })?;
                let stamp = Fingerprint {
                    content_hash: String::new(),
                    size: metadata.len(),
                    mtime_ns: modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as i64,
                };
                let prev = previous.get(path.0.as_str());
                let vector_retry = embedding_on && missing_vectors.contains(path.0.as_str());
                // Skip stat-unchanged files without reading them at all.
                if prev.is_some_and(|p| p.mtime_ns == stamp.mtime_ns && p.size == stamp.size)
                    && !vector_retry
                {
                    return Ok(false);
                }
                // Retrying vectors needs the parsed document again, so re-read it.
                let previous_hash = if vector_retry {
                    None
                } else {
                    prev.map(|p| p.content_hash.clone())
                };
                let read_root = self.root.clone();
                let read_path = path.clone();
                let read = tokio::task::spawn_blocking(move || {
                    read_document_if_changed(&read_root, &read_path, previous_hash.as_deref())
                })
                .await
                .map_err(|e| AgentWikiError::Other(format!("document task failed: {e}")))?;
                let (doc, body) = match read? {
                    // Identical bytes: refresh the observed stamp and skip parsing.
                    DocumentRead::Unchanged(fingerprint) => {
                        self.index
                            .update_document_fingerprint(path, &fingerprint)
                            .await?;
                        return Ok(false);
                    }
                    DocumentRead::Changed(doc, body) => (doc, body),
                };
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
                let vectors = self.vectors_for(path, &slices, &mut report, embed).await?;
                self.index
                    .replace_document(
                        path,
                        &slices,
                        &edges,
                        &vectors,
                        &doc.fingerprint,
                        self.embedding_model,
                    )
                    .await?;
                if prev.is_none()
                    && removed_by_hash
                        .get(&doc.fingerprint.content_hash)
                        .is_some_and(|c| c.len() == 1)
                {
                    report.moved += 1;
                }
                Ok(true)
            }
            .await;
            match outcome {
                Ok(true) => report.indexed += 1,
                Ok(false) => report.unchanged += 1,
                Err(e) => report.degraded.push(format!(
                    "{}: {e}; projection may be stale, retry on next sync",
                    path.0
                )),
            }
        }
        for path in previous.keys().filter(|p| !current.contains(*p)) {
            match self.index.delete_path(&PathScope(path.into())).await {
                Ok(()) => report.removed += 1,
                Err(e) => report
                    .degraded
                    .push(format!("{path}: deletion failed: {e}")),
            }
        }
        // Repair interrupted index builds once rows exist; never fold stale rows
        // into existing indices implicitly.
        if report.indexed + report.removed > 0
            && let Err(error) = self.index.maintain_indexes(rebuild).await
        {
            report
                .degraded
                .push(format!("index maintenance failed: {error}"));
        }
        report.generation = hex::encode(Sha256::digest(
            format!("{:?}", self.index.document_fingerprints().await?).as_bytes(),
        ));
        Ok(report)
    }

    /// Reuse vectors whose exact model/dimension/input hash is already stored and
    /// recompute only the remaining ones in bounded batches.
    async fn vectors_for(
        &self,
        path: &PathScope,
        slices: &[Slice],
        report: &mut SyncReport,
        embed: bool,
    ) -> Result<Vec<Option<Vec<f32>>>> {
        let Some(identity) = self.embedding_model.filter(|_| embed) else {
            return Ok(vec![None; slices.len()]);
        };
        let Some(embedder) = self.embedder().await else {
            report.vectors_pending += slices.len();
            self.embedding_degraded(report);
            return Ok(vec![None; slices.len()]);
        };
        let reusable = match self.index.reusable_vectors(path).await {
            Ok(reusable) => reusable,
            Err(error) => {
                report
                    .degraded
                    .push(format!("{}: reusing vectors failed: {error}", path.0));
                HashMap::new()
            }
        };
        let mut vectors = Vec::with_capacity(slices.len());
        let mut pending = Vec::new();
        for slice in slices {
            match reusable.get(&embedding_input_hash(Some(identity), &slice.search_text)) {
                Some(vector) => vectors.push(Some(vector.clone())),
                None => {
                    vectors.push(None);
                    pending.push(slice.search_text.clone());
                }
            }
        }
        let reused = vectors.iter().filter(|v| v.is_some()).count();
        if !pending.is_empty() {
            let computed = tokio::task::spawn_blocking(move || {
                let mut embedder = embedder
                    .lock()
                    .map_err(|error| AgentWikiError::Embedding(error.to_string()))?;
                embedder
                    .embed(pending, Some(EMBED_BATCH_SIZE))
                    .map_err(AgentWikiError::Embedding)
            })
            .await
            .map_err(|e| AgentWikiError::Other(format!("embedding task failed: {e}")))?;
            match computed {
                Ok(embedded) => {
                    let mut embedded = embedded.into_iter();
                    for slot in &mut vectors {
                        if slot.is_none()
                            && let Some(vector) = embedded.next()
                        {
                            *slot = Some(vector);
                        }
                    }
                }
                Err(error) => {
                    report
                        .degraded
                        .push(format!("{}: embedding unavailable: {error}", path.0));
                    if let Ok(mut current) = self.embedding_error.lock() {
                        *current = Some(error.to_string());
                    }
                }
            }
        }
        report.vectors_reused += reused;
        report.vectors_ready += vectors.iter().filter(|v| v.is_some()).count();
        report.vectors_pending += vectors.iter().filter(|v| v.is_none()).count();
        Ok(vectors)
    }

    pub async fn rebuild(&mut self) -> Result<SyncReport> {
        let lock_path = self.lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_path))
            .await
            .map_err(|e| AgentWikiError::Other(format!("lock task failed: {e}")))??;
        self.index.reset().await?;
        self.sync_locked(true, true).await
    }

    /// Explicit maintenance: repair indices and fold new rows into them.
    pub async fn maintain(&mut self) -> Result<()> {
        let lock_path = self.lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_path))
            .await
            .map_err(|e| AgentWikiError::Other(format!("lock task failed: {e}")))??;
        self.index.maintain_indexes(true).await
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
    file.lock_exclusive().map_err(|source| AgentWikiError::Io {
        path: path.into(),
        source,
    })?;
    Ok(file)
}
