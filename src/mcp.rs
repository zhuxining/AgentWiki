//! MCP composition root (binary `agentwiki-mcp`).
//!
//! Exposes AgentWiki retrieval / rules / validation through the Model Context
//! Protocol over stdio. The actual SDK wiring lives behind the `mcp` feature so
//! the core library stays SDK-free; without that feature the binary still
//! compiles and prints a clear guidance message.

#[cfg(not(feature = "mcp"))]
fn main() {
    eprintln!("agentwiki-mcp requires the `mcp` feature; run with --features mcp.");
}

#[cfg(feature = "mcp")]
#[tokio::main]
async fn main() {
    if let Err(error) = async_main().await {
        eprintln!("agentwiki-mcp: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(feature = "mcp")]
#[derive(Clone)]
struct McpServer {
    wiki: std::sync::Arc<agentwiki::AgentWiki>,
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
    #[serde(default = "default_document_limit")]
    document_limit: usize,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    keyword_mode: agentwiki::KeywordMode,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    note_types: Vec<String>,
    #[serde(default)]
    metadata_filters: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    modified_after_ns: Option<i64>,
    #[serde(default)]
    modified_before_ns: Option<i64>,
    #[serde(default)]
    order: agentwiki::SearchOrder,
    #[serde(default)]
    include_relations: bool,
}

#[cfg(feature = "mcp")]
fn default_limit() -> usize {
    10
}

#[cfg(feature = "mcp")]
fn default_document_limit() -> usize {
    5
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
        let mut query = agentwiki::ContextQuery::default();
        query.query = args.query.clone();
        query.scope = args.scope.clone();
        query.document_limit = args.document_limit;
        query.fragment_limit = args.limit;
        query.keywords = args.keywords.clone();
        query.keyword_mode = args.keyword_mode;
        query.tags = args.tags.clone();
        query.note_types = args.note_types.clone();
        query.metadata_filters = args.metadata_filters.clone();
        query.modified_after_ns = args.modified_after_ns;
        query.modified_before_ns = args.modified_before_ns;
        query.order = args.order;
        query.include_relations = args.include_relations;
        let result = self
            .wiki
            .query(query.clone())
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
        // Assemble the contractual response shape (MCP_TOOLS.md),
        // enriching each ranked slice with filename / summary / mtime / frontmatter.
        let documents = result
            .documents
            .iter()
            .map(|hit| {
                serde_json::json!({
                    "path": hit.slice.path.0,
                    "type": hit.slice.note_type,
                    "filename": hit.filename,
                    "summary": hit.slice.content,
                    "tags": hit.slice.tags,
                    "rank_score": hit.score,
                    "match_sources": hit.sources,
                    "modified_at": rfc3339_from_nanos(hit.modified_at_ns),
                    "frontmatter": hit.frontmatter,
                })
            })
            .collect::<Vec<_>>();
        let fragments = result
            .fragments
            .iter()
            .map(|hit| {
                serde_json::json!({
                    "path": hit.slice.path.0,
                    "type": hit.slice.note_type,
                    "filename": hit.filename,
                    "section": hit.slice.section,
                    "snippet": hit.slice.content,
                    "tags": hit.slice.tags,
                    "rank_score": hit.score,
                    "match_sources": hit.sources,
                    "modified_at": rfc3339_from_nanos(hit.modified_at_ns),
                    "frontmatter": hit.frontmatter,
                })
            })
            .collect::<Vec<_>>();
        let truncated =
            documents.len() >= query.document_limit || fragments.len() >= query.fragment_limit;
        serde_json::to_string(&serde_json::json!({
            "query": args.query,
            "scope": args.scope,
            "strategy": result.strategy,
            "degraded": result.degraded,
            "documents": documents,
            "fragments": fragments,
            "relations": result.related,
            "truncated": truncated,
        }))
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
    }

    #[rmcp::tool(
        name = "validate_wiki",
        description = "Validate Markdown, frontmatter, links, and AgentWiki rules."
    )]
    async fn validate_wiki(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<ValidateArgs>,
    ) -> Result<String, rmcp::ErrorData> {
        let scope = match (args.path, args.full) {
            (Some(path), false) => {
                agentwiki::ValidationScope::Document(agentwiki::PathScope(path.into()))
            }
            (None, true) => agentwiki::ValidationScope::All,
            _ => {
                return Err(rmcp::ErrorData::invalid_params(
                    "specify exactly one of path or full",
                    None,
                ));
            }
        };
        self.wiki
            .validate(agentwiki::ValidationRequest {
                scope,
                fix_format: args.fix_format,
            })
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
            .and_then(|mut result| {
                let root = self.wiki.wiki_root().to_path_buf();
                for issue in &mut result.issues {
                    issue.path = root.join(&issue.path).to_string();
                }
                for path in &mut result.formatted_paths {
                    *path = root.join(&*path).to_string();
                }
                serde_json::to_string(&result)
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
            })
    }

    #[rmcp::tool(
        name = "get_wiki_rules",
        description = "Return the reserved AGENTWIKI.md rule document."
    )]
    async fn get_wiki_rules(
        &self,
        rmcp::handler::server::wrapper::Parameters(args): rmcp::handler::server::wrapper::Parameters<RulesArgs>,
    ) -> Result<String, rmcp::ErrorData> {
        let result = self
            .wiki
            .rules(agentwiki::RulesRequest { scope: args.scope })
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
        serde_json::to_string(&result)
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
    }
}

#[cfg(feature = "mcp")]
async fn async_main() -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let config = agentwiki::config::AppConfig::load()?;
    let wiki_root = config.wiki_root.clone();
    let projection_dir = config.projection_for_root(&wiki_root)?;
    let embedding_model = config.embedding_model.clone();
    let wiki = agentwiki::AgentWiki::open(agentwiki::OpenOptions {
        wiki_root,
        projection_dir,
        embedding_model,
    })
    .await?;
    let server = McpServer {
        wiki: std::sync::Arc::new(wiki),
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
