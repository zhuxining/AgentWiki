//! CLI composition root (binary `agentwiki`).
//!
//! Reads the user config (`~/.agentwiki/config.json`), assembles a [`Runtime`]
//! and routes subcommands: query, sync-index, rebuild-index, validate-wiki.
//! Only this root reads global config; library modules receive parsed values.

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

use agentwiki::config::AppConfig;
use agentwiki::model::ContextQuery;

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

fn main() {
    // Application boundary: convert library errors to a concise exit path.
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let cfg = AppConfig::load()?;
    let root = cli.wiki_root.clone().unwrap_or(cfg.wiki_root.clone());

    if matches!(cli.command, Command::ShowConfig) {
        print_config(&cli, &cfg, &root);
        return Ok(());
    }

    // Ensure the wiki root exists before assembly (runtime seeds AGENTWIKI.md).
    std::fs::create_dir_all(&root).map_err(|e| anyhow::anyhow!("create root {root}: {e}"))?;

    let mut rt = agentwiki::Runtime::assemble_with_embedding(
        &root,
        &cfg.projection_for_root(&root)?,
        cfg.embedding_model.as_deref(),
    )?;

    match cli.command {
        Command::Query {
            query,
            limit,
            scope,
        } => {
            // Prefer to serve a fresh projection on-demand.
            let q = ContextQuery {
                query,
                scope,
                limit,
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
                Some(p) => {
                    let scope =
                        agentwiki::document::scope_path(&root, &camino::Utf8PathBuf::from(&p))?;
                    Some(scope)
                }
                None => None,
            };
            let result = rt.validate_with_format(scope.as_ref(), scope.is_none(), fix_format)?;
            for path in result.formatted_paths {
                println!("formatted {path}");
            }
            let issues = result.issues;
            if issues.is_empty() {
                println!("no issues");
            } else {
                for i in issues {
                    let level = match i.severity {
                        agentwiki::model::Severity::Error => "ERROR",
                        agentwiki::model::Severity::Warning => "WARNING",
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
