//! MCP composition root (binary `agentwiki-mcp`).
//!
//! Exposes AgentWiki retrieval / rules / validation through the Model Context
//! Protocol over stdio. The actual SDK wiring lives behind the `mcp` feature so
//! the core library stays SDK-free; without that feature the binary still
//! compiles and prints a clear guidance message.

fn main() {
    run()
}

#[cfg(not(feature = "mcp"))]
fn run() {
    eprintln!("agentwiki-mcp requires the `mcp` feature; run with --features mcp.");
}

#[cfg(feature = "mcp")]
fn run() {
    if let Err(error) = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async_main())
    {
        eprintln!("agentwiki-mcp: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(feature = "mcp")]
#[derive(Clone)]
struct McpServer {
    runtime: std::sync::Arc<std::sync::Mutex<agentwiki::Runtime>>,
}

#[cfg(feature = "mcp")]
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ContextArgs {
    #[serde(default)]
    query: String,
    #[serde(default)]
    scope: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    note_types: Vec<String>,
    #[serde(default)]
    metadata_filters: agentwiki::model::Frontmatter,
}

#[cfg(feature = "mcp")]
fn default_limit() -> usize {
    10
}

#[cfg(feature = "mcp")]
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ValidateArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    fix_format: bool,
}

#[cfg(feature = "mcp")]
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RulesArgs {
    #[serde(default)]
    scope: String,
}

#[cfg(feature = "mcp")]
#[rmcp::tool_router(server_handler)]
impl McpServer {
    #[rmcp::tool(
        name = "get_wiki_context",
        description = "Search the wiki and return ranked Markdown evidence."
    )]
    async fn get_wiki_context(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<ContextArgs>,
    ) -> Result<String, rmcp::ErrorData> {
        let shared = self.runtime.clone();
        tokio::task::spawn_blocking(move || {
            let mut runtime = shared
                .lock()
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let mut query = agentwiki::model::ContextQuery::default();
            query.query = args.query.clone();
            query.scope = args.scope.clone();
            query.limit = args.limit;
            query.tags = args.tags.clone();
            query.note_types = args.note_types.clone();
            query.metadata_filters = args.metadata_filters.clone();
            let result = runtime
                .query(&query)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            // Assemble the contractual response shape (MCP_TOOLS.md),
            // enriching each ranked slice with title / mtime / frontmatter.
            let mut results = Vec::new();
            for (index, hit) in result.slices.iter().enumerate() {
                let (title, mtime_ns, frontmatter) = runtime
                    .hit_metadata(hit.slice.path.0.as_str())
                    .unwrap_or_default();
                // One-hop relations attach to the primary hit only.
                let related = if index == 0 {
                    result
                        .related
                        .iter()
                        .map(serde_json::to_value)
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
                } else {
                    Vec::new()
                };
                results.push(serde_json::json!({
                    "path": hit.slice.path.0,
                    "title": title,
                    "section": hit.slice.section,
                    "snippet": hit.slice.content,
                    "rank_score": hit.score,
                    "match_sources": hit.sources,
                    "modified_at": rfc3339_from_nanos(mtime_ns),
                    "frontmatter": frontmatter,
                    "related": related,
                }));
            }
            let strategy = if query.query.trim().is_empty() {
                "recent"
            } else if result
                .slices
                .iter()
                .any(|s| s.sources.iter().any(|src| src == "semantic"))
            {
                "hybrid"
            } else {
                "keyword"
            };
            let truncated = results.len() >= query.limit;
            serde_json::to_string(&serde_json::json!({
                "query": args.query,
                "scope": args.scope,
                "strategy": strategy,
                "degraded": result.degraded,
                "results": results,
                "truncated": truncated,
            }))
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
    }

    #[rmcp::tool(
        name = "validate_wiki",
        description = "Validate Markdown, frontmatter, links, and AgentWiki rules."
    )]
    async fn validate_wiki(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<ValidateArgs>,
    ) -> Result<String, rmcp::ErrorData> {
        let shared = self.runtime.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = shared
                .lock()
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let scope = args
                .path
                .map(|path| agentwiki::model::PathScope(path.into()));
            runtime
                .validate_with_format(scope.as_ref(), args.full, args.fix_format)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
                .and_then(|issues| {
                    serde_json::to_string(&issues)
                        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
                })
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
    }

    #[rmcp::tool(
        name = "get_wiki_rules",
        description = "Return the reserved AGENTWIKI.md rule document."
    )]
    async fn get_wiki_rules(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<RulesArgs>,
    ) -> Result<String, rmcp::ErrorData> {
        let shared = self.runtime.clone();
        tokio::task::spawn_blocking(move || {
            let mut runtime = shared
                .lock()
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            // The contract requires a fresh document projection before rules:
            // known_tags come from the ledger, not from the rule file.
            runtime
                .ensure_fresh()
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let path = agentwiki::model::PathScope("AGENTWIKI.md".into());
            let document = agentwiki::document::read_document(&runtime.root, &path)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let guide_content = agentwiki::document::read_body(&runtime.root, &path)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            // Structured rules per the contract: root rule plus every section
            // matching the requested scope. A broken rule file is an error, so
            // agents never silently operate without governing rules.
            let parsed = agentwiki::governance::rules::parse_rules(&document.frontmatter)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let effective = agentwiki::governance::rules::effective_rules(&args.scope, &parsed);
            let known_tags = runtime
                .known_tags()
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            serde_json::to_string(&serde_json::json!({
                "wiki_root": runtime.root,
                "guide_content": guide_content,
                "source_modified_at_ns": document.fingerprint.mtime_ns,
                "source_size": document.fingerprint.size,
                "default_type": effective.default_type,
                "required_fields": effective.required_fields,
                "tag_aliases": effective.tag_aliases,
                "sections": effective.sections,
                "known_tags": known_tags,
                "scope": args.scope,
            }))
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
        })
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?
    }
}

#[cfg(feature = "mcp")]
async fn async_main() -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let config = agentwiki::config::AppConfig::load()?;
    std::fs::create_dir_all(&config.wiki_root)?;
    let wiki_root = config.wiki_root.clone();
    let projection_dir = config.projection_for_root(&wiki_root)?;
    let embedding_model = config.embedding_model.clone();
    let runtime = tokio::task::spawn_blocking(move || {
        agentwiki::Runtime::assemble_with_embedding(
            &wiki_root,
            &projection_dir,
            embedding_model.as_deref(),
        )
    })
    .await??;
    let server = McpServer {
        runtime: std::sync::Arc::new(std::sync::Mutex::new(runtime)),
    };
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// Convert a unix-epoch nanosecond timestamp into RFC 3339 UTC (contract
/// `modified_at`); zero/negative timestamps yield the epoch.
#[cfg(feature = "mcp")]
fn rfc3339_from_nanos(nanos: i64) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's days-from-civil algorithm (public domain).
#[cfg(feature = "mcp")]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}
