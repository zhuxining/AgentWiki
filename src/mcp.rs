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
    // The SDK-backed stdio server is wired here. It reuses the library's
    // Runtime for get_wiki_context / get_wiki_rules / validate_wiki.
    // (Intent: build the server shell that compiles under `--features mcp`.)
    eprintln!("agentwiki-mcp: mcp feature enabled (server wiring in progress)");
}
