use std::collections::HashMap;

use grepify::ops::code::{match_code as sdk_match_code, CodeMatch};
use napi_derive::napi;

use crate::chunk::ChunkJs;

#[napi(object)]
#[derive(Clone, Debug)]
pub struct CodeMatchJs {
    pub kind: String,
    pub chunks: Vec<ChunkJs>,
    pub captures: HashMap<String, Vec<ChunkJs>>,
}

impl CodeMatchJs {
    fn from_match(m: &CodeMatch, source: &str) -> Self {
        let chunks = m
            .chunks
            .iter()
            .map(ChunkJs::from_chunk)
            .collect::<Vec<_>>();
        let captures = m
            .captures
            .iter()
            .map(|(name, cs)| (name.clone(), cs.iter().map(ChunkJs::from_chunk).collect()))
            .collect();
        let _ = source; // text() available on JS side via slice
        Self {
            kind: m.kind.to_string(),
            chunks,
            captures,
        }
    }
}

#[napi]
pub fn match_code(pattern: String, source: String, language: String) -> napi::Result<Vec<CodeMatchJs>> {
    let matches = sdk_match_code(&pattern, &source, &language)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(matches
        .iter()
        .map(|m| CodeMatchJs::from_match(m, &source))
        .collect())
}

#[napi]
pub fn index_terms(
    source: String,
    language: String,
    #[napi(ts_arg_type = "number")] min_len: u32,
) -> napi::Result<Vec<String>> {
    grepify::ops::code::index_terms(&source, &language, min_len as usize)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}
