use grepify::resources::chunk::Chunk;
use napi_derive::napi;

// Exposed in the generated `.d.ts` as `TextPosition` (a stable public shape),
// even though the current bindings return the flattened `ChunkJs`.
#[allow(dead_code)]
#[napi(object)]
#[derive(Clone, Debug)]
pub struct TextPositionJs {
    pub byte_offset: u32,
    pub char_offset: u32,
    pub line: u32,
    pub column: u32,
}

#[napi(object)]
#[derive(Clone, Debug)]
pub struct ChunkJs {
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_char_offset: u32,
    pub start_line: u32,
    pub start_column: u32,
    pub end_char_offset: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl ChunkJs {
    pub fn from_chunk(chunk: &Chunk) -> Self {
        let range = chunk.range();
        Self {
            start_byte: range.start as u32,
            end_byte: range.end as u32,
            start_char_offset: chunk.start.char_offset as u32,
            start_line: chunk.start.line,
            start_column: chunk.start.column,
            end_char_offset: chunk.end.char_offset as u32,
            end_line: chunk.end.line,
            end_column: chunk.end.column,
        }
    }
}
