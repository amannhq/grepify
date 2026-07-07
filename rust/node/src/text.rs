use grepify::ops::text::{RecursiveChunkConfig, RecursiveSplitter, detect_code_language};
use napi_derive::napi;

use crate::chunk::ChunkJs;

#[napi(object)]
#[derive(Clone, Debug)]
pub struct RecursiveChunkConfigJs {
    pub chunk_size: u32,
    pub min_chunk_size: Option<u32>,
    pub chunk_overlap: Option<u32>,
    pub language: Option<String>,
}

#[napi]
pub fn detect_code_language_js(filename: String) -> Option<String> {
    detect_code_language(&filename)
}

#[napi]
pub fn split_text_recursive(
    source: String,
    config: RecursiveChunkConfigJs,
) -> napi::Result<Vec<ChunkJs>> {
    let splitter = RecursiveSplitter::new().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let chunks = splitter.split_with(
        &source,
        RecursiveChunkConfig {
            chunk_size: config.chunk_size as usize,
            min_chunk_size: config.min_chunk_size.map(|n| n as usize),
            chunk_overlap: config.chunk_overlap.map(|n| n as usize),
            language: config.language,
        },
    );
    Ok(chunks.iter().map(ChunkJs::from_chunk).collect())
}
