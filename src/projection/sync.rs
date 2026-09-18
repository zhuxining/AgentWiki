//! Incremental Markdown -> LanceDB projection.

use crate::document::types::{Fingerprint, PathScope};
use crate::error::{AgentWikiError, Result};
use crate::projection::types::SyncReport;
use crate::{
    document,
    document::relation,
    projection::{LanceIndex, embedding::Embedder},
};
use camino::Utf8Path;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

const EMBEDDING_IDENTITY: &str = "BAAI/bge-small-zh-v1.5";

pub struct Projection {
    pub root: camino::Utf8PathBuf,
    pub index: LanceIndex,
    pub embedder: Option<Arc<Mutex<Embedder>>>,
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
            .map_err(|e| AgentWikiError::Other(format!("lock task failed: {e}")))??;
        let initialized = match model {
            Some(_) => Some(
                tokio::task::spawn_blocking(Embedder::bge_small_zh)
                    .await
                    .map_err(|e| AgentWikiError::Other(format!("embedding task failed: {e}")))?,
            ),
            None => None,
        };
        let (embedder, embedding_error) = match initialized {
            Some(Ok(e)) => (Some(Arc::new(Mutex::new(e))), None),
            Some(Err(e)) => (None, Some(e)),
            None => (None, None),
        };
        let index = LanceIndex::open(index_dir, model.map(|_| 512)).await?;
        Ok(Self {
            root: root.into(),
            index,
            embedder,
            embedding_error,
            lock_path,
        })
    }

    pub async fn ensure_fresh(&mut self) -> Result<SyncReport> {
        let lock_path = self.lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_path))
            .await
            .map_err(|e| AgentWikiError::Other(format!("lock task failed: {e}")))??;
        self.sync_locked().await
    }

    async fn sync_locked(&mut self) -> Result<SyncReport> {
        let root = self.root.clone();
        let paths = tokio::task::spawn_blocking(move || document::snapshot(&root))
            .await
            .map_err(|e| AgentWikiError::Other(format!("scan task failed: {e}")))??;
        let current: BTreeSet<String> = paths.iter().map(|p| p.0.to_string()).collect();
        let previous = self.index.document_fingerprints().await?;
        let missing_vectors = if self.embedder.is_some() {
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
                let vector_retry = missing_vectors.contains(path.0.as_str());
                if prev.is_some_and(|p| p.mtime_ns == stamp.mtime_ns && p.size == stamp.size)
                    && !vector_retry
                {
                    return Ok(false);
                }
                let read_root = self.root.clone();
                let read_path = path.clone();
                let (doc, body) = tokio::task::spawn_blocking(move || {
                    document::parse::read_document_with_body(&read_root, &read_path)
                })
                .await
                .map_err(|e| AgentWikiError::Other(format!("document task failed: {e}")))??;
                if prev.is_some_and(|p| p.content_hash == doc.fingerprint.content_hash) {
                    if vector_retry {
                        // Keep the existing lexical projection and retry only embedding below.
                    } else {
                        self.index
                            .update_document_fingerprint(path, &doc.fingerprint)
                            .await?;
                        return Ok(false);
                    }
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
                let vectors = if let Some(embedder) = &self.embedder {
                    let inputs = slices
                        .iter()
                        .map(|s| s.search_text.clone())
                        .collect::<Vec<_>>();
                    let embedder = embedder.clone();
                    match tokio::task::spawn_blocking(move || {
                        let mut e = embedder
                            .lock()
                            .map_err(|x| AgentWikiError::Embedding(x.to_string()))?;
                        e.embed(inputs).map_err(AgentWikiError::Embedding)
                    })
                    .await
                    .map_err(|e| AgentWikiError::Other(format!("embedding task failed: {e}")))?
                    {
                        Ok(v) => {
                            report.vectors_ready += v.len();
                            Some(v)
                        }
                        Err(e) => {
                            report.vectors_pending += slices.len();
                            report
                                .degraded
                                .push(format!("{}: embedding unavailable: {e}", path.0));
                            None
                        }
                    }
                } else {
                    None
                };
                self.index
                    .replace_document(
                        path,
                        &slices,
                        &edges,
                        vectors.as_deref(),
                        &doc.fingerprint,
                        self.embedder.as_ref().map(|_| EMBEDDING_IDENTITY),
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
        report.generation = hex::encode(Sha256::digest(
            format!("{:?}", self.index.document_fingerprints().await?).as_bytes(),
        ));
        Ok(report)
    }

    pub async fn rebuild(&mut self) -> Result<SyncReport> {
        let lock_path = self.lock_path.clone();
        let _lock = tokio::task::spawn_blocking(move || acquire_lock(&lock_path))
            .await
            .map_err(|e| AgentWikiError::Other(format!("lock task failed: {e}")))??;
        self.index.reset().await?;
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
    file.lock_exclusive().map_err(|source| AgentWikiError::Io {
        path: path.into(),
        source,
    })?;
    Ok(file)
}
