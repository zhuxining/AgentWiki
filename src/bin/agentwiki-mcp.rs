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
            query.query = args.query;
            query.scope = args.scope;
            query.limit = args.limit;
            query.tags = args.tags;
            query.note_types = args.note_types;
            query.metadata_filters = args.metadata_filters;
            runtime
                .query(&query)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
                .and_then(|result| {
                    serde_json::to_string(&result)
                        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))
                })
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
    async fn get_wiki_rules(&self) -> Result<String, rmcp::ErrorData> {
        let shared = self.runtime.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = shared
                .lock()
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let path = agentwiki::model::PathScope("AGENTWIKI.md".into());
            let document = agentwiki::document::read_document(&runtime.root, &path)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let guide_content = agentwiki::document::read_body(&runtime.root, &path)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            serde_json::to_string(&serde_json::json!({
                "wiki_root": runtime.root,
                "frontmatter": document.frontmatter,
                "guide_content": guide_content,
                "source_modified_at_ns": document.fingerprint.mtime_ns,
                "source_size": document.fingerprint.size,
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
