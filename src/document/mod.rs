//! Markdown document boundary: safe paths, parsing, chunking and snapshots.

pub mod chunk;
pub mod parse;
pub mod path;

pub use chunk::chunk_document;
pub use parse::{DEFAULT_AGENTWIKI, parse_frontmatter, read_body, read_document};
pub use path::{scope_path, snapshot};
