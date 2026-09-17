//! Markdown document boundary: safe paths, parsing, chunking and snapshots.

pub mod chunk;
pub mod parse;
pub mod path;
pub mod relation;
pub mod types;

pub use chunk::chunk_document;
pub use parse::{DEFAULT_AGENTWIKI, parse_frontmatter};
pub use path::{scope_path, snapshot};
