//! CLI composition root (binary `agentwiki`).
//!
//! Reads the user config (`~/.agentwiki/config.json`), assembles [`AgentWiki`]
//! and routes subcommands: query, sync-index, rebuild-index, validate-wiki.
//! Only this root reads global config; library modules receive parsed values.

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

use agentwiki::config::AppConfig;
use agentwiki::{
    AgentWiki, ContextQuery, KeywordMode, OpenOptions, PathScope, SearchOrder, Severity,
    ValidationRequest, ValidationScope,
};

/// Subcommand routing.
#[derive(Parser)]
#[command(
    name = "agentwiki",
    version,
    about = "Local-first Markdown knowledge base context retrieval"
)]
struct Cli {
    /// Wiki root directory (overrides config `wiki_root`).
    #[arg(long)]
    wiki_root: Option<Utf8PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Retrieve task context (keyword + optional semantic hit).
    Query {
        /// Free-text query; empty lists recently modified documents.
        query: String,
        /// Limit fragment results returned (1..=20).
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Limit document results returned (1..=20).
        #[arg(long, default_value_t = 5)]
        document_limit: usize,
        /// Restrict to a wiki-relative path scope.
        #[arg(long, default_value_t = String::new())]
        scope: String,
        /// Require this tag (repeatable; all tags must be present).
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        /// Require one of these document types (repeatable).
        #[arg(long = "note-type", value_name = "TYPE")]
        note_types: Vec<String>,
        /// Explicit keyword (repeatable).
        #[arg(long = "keyword", value_name = "TERM")]
        keywords: Vec<String>,
        /// Require all explicit keywords instead of any keyword.
        #[arg(long)]
        keyword_all: bool,
        /// Frontmatter equality filter KEY=VALUE (repeatable; all must hold).
        #[arg(long = "metadata", value_name = "KEY=VALUE")]
        metadata: Vec<String>,
        /// Only include files modified at or after this unix nanosecond value.
        #[arg(long)]
        modified_after_ns: Option<i64>,
        /// Only include files modified at or before this unix nanosecond value.
        #[arg(long)]
        modified_before_ns: Option<i64>,
        /// Sort by modified time instead of relevance.
        #[arg(long)]
        modified_desc: bool,
        /// Return one-hop relation metadata for the primary result.
        #[arg(long)]
        include_relations: bool,
    },
    /// Reconcile the index/metadata projections with the wiki.
    SyncIndex,
    /// Wipe derived projections and rebuild from Markdown.
    RebuildIndex,
    /// Fold new rows into existing LanceDB indices (explicit maintenance).
    OptimizeIndex,
    /// Validate Markdown, frontmatter and internal links. Reports only.
    ValidateWiki {
        /// Validate a single wiki-relative path; omit to validate the whole wiki.
        #[arg(long)]
        path: Option<String>,
        /// Validate the whole wiki explicitly (required for full formatting).
        #[arg(long)]
        full: bool,
        /// Rewrite Markdown formatting after checking it.
        #[arg(long)]
        fix_format: bool,
    },
    /// Print the resolved configuration.
    ShowConfig,
}

#[tokio::main]
async fn main() {
    // Application boundary: convert library errors to a concise exit path.
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let cfg = AppConfig::load()?;
    let root = cli.wiki_root.clone().unwrap_or(cfg.wiki_root.clone());

    if matches!(cli.command, Command::ShowConfig) {
        print_config(&cli, &cfg, &root);
        return Ok(());
    }

    let wiki = AgentWiki::open(OpenOptions {
        wiki_root: root.clone(),
        projection_dir: cfg.projection_for_root(&root)?,
        embedding_model: cfg.embedding_model.clone(),
    })
    .await?;

    match cli.command {
        Command::Query {
            query,
            limit,
            document_limit,
            scope,
            tags,
            note_types,
            keywords,
            keyword_all,
            metadata,
            modified_after_ns,
            modified_before_ns,
            modified_desc,
            include_relations,
        } => {
            // Prefer to serve a fresh projection on-demand.
            let mut metadata_filters = serde_json::Map::new();
            for pair in &metadata {
                let Some((key, value)) = pair.split_once('=') else {
                    anyhow::bail!("--metadata expects KEY=VALUE, got `{pair}`");
                };
                let value = serde_json::from_str(value)
                    .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
                metadata_filters.insert(key.to_string(), value);
            }
            let q = ContextQuery {
                query,
                scope,
                document_limit,
                fragment_limit: limit,
                keywords,
                keyword_mode: if keyword_all {
                    KeywordMode::All
                } else {
                    KeywordMode::Any
                },
                tags,
                note_types,
                metadata_filters,
                modified_after_ns,
                modified_before_ns,
                order: if modified_desc {
                    SearchOrder::ModifiedDesc
                } else {
                    SearchOrder::Relevance
                },
                include_relations,
            };
            let res = wiki.query(q).await?;
            for hit in &res.documents {
                println!(
                    "DOC {} {:.4}{}",
                    hit.slice.path.0,
                    hit.score,
                    snippet(&hit.slice.content)
                );
            }
            for hit in &res.fragments {
                let title = hit.slice.section.clone();
                println!(
                    "{} [{}]{}{}",
                    hit.slice.path.0,
                    title,
                    if hit.score > 0.0 {
                        format!(" {:.4}", hit.score)
                    } else {
                        String::new()
                    },
                    snippet(&hit.slice.content)
                );
            }
            println!(
                "{} documents; {} fragments; degraded={}",
                res.documents.len(),
                res.fragments.len(),
                res.degraded.len()
            );
        }
        Command::SyncIndex => {
            let report = wiki.sync().await?;
            print_report(&report);
        }
        Command::RebuildIndex => {
            let report = wiki.rebuild().await?;
            print_report(&report);
        }
        Command::OptimizeIndex => {
            wiki.maintain_indexes().await?;
            println!("indexes repaired and updated");
        }
        Command::ValidateWiki {
            path,
            full,
            fix_format,
        } => {
            if path.is_none() && !full && fix_format {
                anyhow::bail!("--fix-format requires --path or --full");
            }
            if path.is_some() && full {
                anyhow::bail!("--path and --full are mutually exclusive");
            }
            let scope = match path {
                Some(path) => ValidationScope::Document(PathScope(path.into())),
                None => ValidationScope::All,
            };
            let result = wiki
                .validate(ValidationRequest { scope, fix_format })
                .await?;
            for path in result.formatted_paths {
                println!("formatted {path}");
            }
            let issues = result.issues;
            if issues.is_empty() {
                println!("no issues");
            } else {
                for i in issues {
                    let level = match i.severity {
                        Severity::Error => "ERROR",
                        Severity::Warning => "WARNING",
                    };
                    println!("{level} {}:{} {i}", i.kind, i.path);
                }
            }
        }
        Command::ShowConfig => unreachable!(),
    }
    Ok(())
}

/// Print the resolved configuration, annotating where each value came from.
fn print_config(cli: &Cli, cfg: &AppConfig, root: &Utf8PathBuf) {
    let config_file = cfg
        .config_file
        .as_deref()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "(none — created default)".to_owned());
    println!("config_file = {config_file}");
    println!(
        "wiki_root = {root} ({})",
        if cli.wiki_root.is_some() {
            "cli"
        } else if cfg.config_file.is_some() {
            "config"
        } else {
            "default"
        }
    );
    match &cfg.embedding_model {
        Some(model) => println!("embedding_model = {model}"),
        None => println!("embedding_model = null (semantic leg off)"),
    }
    println!("projection_dir = {}", cfg.projection_dir);
}

/// First N chars of content for a one-line console snippet.
fn snippet(content: &str) -> String {
    let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = compact.chars().take(80).collect();
    if compact.chars().count() > 80 {
        out.push('…');
    }
    if out.is_empty() {
        String::new()
    } else {
        format!(" :: {out}")
    }
}

fn print_report(r: &agentwiki::SyncReport) {
    println!(
        "indexed={} removed={} moved={} unchanged={} vectors_pending={} degraded={}",
        r.indexed,
        r.removed,
        r.moved,
        r.unchanged,
        r.vectors_pending,
        r.degraded.len()
    );
    for d in &r.degraded {
        println!("  degraded: {d}");
    }
}
