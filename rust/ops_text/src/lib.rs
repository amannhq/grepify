//! Text processing operations for Grepify.
//!
//! This crate provides text processing functionality including:
//! - Text splitting by separators
//! - Recursive text chunking with syntax awareness
//!
//! Language detection / the tree-sitter registry and the shared
//! [`CodeSource`](grepify_code_ast::CodeSource) input type live in the
//! `grepify_code_ast` crate.

pub mod pattern_matcher;
pub mod split;
