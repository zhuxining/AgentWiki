//! CLI composition root (binary `agentwiki`).
//!
//! Reads the user config (`~/.agentwiki/config.json`), assembles a [`Runtime`]
//! and routes subcommands: query, sync-index, rebuild-index, validate-wiki.
//! Only this root reads global config; library modules receive parsed values.

use std::path::PathBuf;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

use agentwiki::model::ContextQuery;

/// Subcommand routing.
#[derive(Parser)]
#[command(
    name = "agentwiki",
    version,
    about = "Local-first Markdown knowledge base context retrieval"
)]
struct Cli {
    /// Skip config file and require explicit --wiki-root / --index-dir.
    #[arg(long, default_value_t = false)]
    no_config: bool,

    /// Wiki root directory (overrides config `document_root`).
    #[arg(long)]
    wiki_root: Option<Utf8PathBuf>,

    /// Directory holding the derived index + metadata store (overrides config).
    #[arg(long)]
    index_dir: Option<Utf8PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Retrieve task context (keyword + optional semantic hit).
    Query {
        /// Free-text query; empty lists recently modified documents.
        query: String,
        /// Limit slices returned (1..=20).
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Restrict to a wiki-relative path scope.
        #[arg(long, default_value_t = String::new())]
        scope: String,
    },
    /// Reconcile the index/metadata projections with the wiki.
    SyncIndex,
    /// Wipe derived projections and rebuild from Markdown.
    RebuildIndex,
    /// Validate Markdown, frontmatter and internal links. Reports only.
    ValidateWiki {
        /// Validate a single wiki-relative path; omit to validate the whole wiki.
        #[arg(long)]
        path: Option<String>,
    },
    /// Print the resolved configuration.
    ShowConfig,
}

fn main() {
    // Application boundary: convert library errors to a concise exit path.
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve wiki root + index dir: CLI flags win, then config file, then
    // defaults (document_root = ~/AgentWiki, index at ~/.agentwiki).
    let (root, index_dir) = if cli.no_config {
        (
            required(&cli.wiki_root, "--wiki-root")?,
            required(&cli.index_dir, "--index-dir")?,
        )
    } else {
        let cfg = Config::load()?;
        let root = cli.wiki_root.clone().unwrap_or(cfg.document_root);
        let index = cli.index_dir.clone().unwrap_or(cfg.index_path);
        (root, index)
    };

    if matches!(cli.command, Command::ShowConfig) {
        println!("wiki_root = {root}");
        println!("index_dir = {index_dir}");
        return Ok(());
    }

    // Ensure the wiki root exists before assembly (runtime seeds AGENTWIKI.md).
    std::fs::create_dir_all(&root).map_err(|e| anyhow::anyhow!("create root {root}: {e}"))?;

    let mut rt = agentwiki::Runtime::assemble(&root, &index_dir)?;

    match cli.command {
        Command::Query {
            query,
            limit,
            scope,
        } => {
            // Prefer to serve a fresh projection on-demand.
            let report = rt.ensure_fresh()?;
            let _ = report;

            let q = ContextQuery {
                query,
                scope,
                limit: limit.clamp(1, 20),
                ..Default::default()
            };
            let res = rt.query(&q)?;
            for hit in &res.slices {
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
            println!("{} hits; degraded={}", res.slices.len(), res.degraded.len());
        }
        Command::SyncIndex => {
            let report = rt.ensure_fresh()?;
            print_report(&report);
        }
        Command::RebuildIndex => {
            let report = rt.rebuild()?;
            print_report(&report);
        }
        Command::ValidateWiki { path } => {
            let scope = match path {
                Some(p) => {
                    let scope =
                        agentwiki::markdown::scope_path(&root, &camino::Utf8PathBuf::from(&p))?;
                    Some(scope)
                }
                None => None,
            };
            let issues = rt.validate(scope.as_ref())?;
            if issues.is_empty() {
                println!("no issues");
            } else {
                for i in issues {
                    println!("{}:{} {i}", i.kind, i.path);
                }
            }
        }
        Command::ShowConfig => unreachable!(),
    }
    Ok(())
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

fn print_report(r: &agentwiki::model::SyncReport) {
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

fn required<T: Clone>(v: &Option<T>, flag: &str) -> anyhow::Result<T> {
    v.clone()
        .ok_or_else(|| anyhow::anyhow!("{flag} is required when --no-config is set"))
}

// ---------------------------------------------------------------------------
// Config (composition-root only)
// ---------------------------------------------------------------------------

/// User configuration loaded from `~/.agentwiki/config.json`.
#[derive(Debug, Clone, serde::Deserialize)]
struct Config {
    #[serde(default = "default_root")]
    document_root: Utf8PathBuf,
    #[serde(default = "default_index")]
    index_path: Utf8PathBuf,
}

fn default_root() -> Utf8PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    Utf8PathBuf::from_path_buf(home.join("AgentWiki"))
        .unwrap_or_else(|_| Utf8PathBuf::from("AgentWiki"))
}

fn default_index() -> Utf8PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    Utf8PathBuf::from_path_buf(home.join(".agentwiki"))
        .unwrap_or_else(|_| Utf8PathBuf::from(".agentwiki"))
}

impl Config {
    /// Load config, creating a default file on first run (never fails hard on
    /// an absent file — falls back to defaults and writes a seed).
    fn load() -> anyhow::Result<Config> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let dir = home.join(".agentwiki");
        let path = dir.join("config.json");

        if !path.exists() {
            let cfg = Config {
                document_root: default_root(),
                index_path: default_index(),
            };
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
            return Ok(cfg);
        }

        let raw = std::fs::read_to_string(&path)?;
        let cfg: Config = serde_json::from_str(&raw)?;
        Ok(cfg)
    }
}

impl serde::Serialize for Config {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Config", 2)?;
        st.serialize_field("document_root", self.document_root.as_str())?;
        st.serialize_field("index_path", self.index_path.as_str())?;
        st.end()
    }
}
