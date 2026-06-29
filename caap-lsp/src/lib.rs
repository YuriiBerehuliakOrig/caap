//! Library surface of the CAAP Language Server.
//!
//! The binary in `main.rs` wires these modules to an `lsp-server` stdio loop.
//! Tests and downstream embedders use them directly without going through the
//! LSP wire protocol.

pub mod analyze;
pub mod doc;
pub mod format;
pub mod index;
pub mod semantic_tokens;
pub mod structure;
pub mod symbols;
pub mod vocab;
