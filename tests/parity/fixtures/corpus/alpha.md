# Alpha document

Grepify is a lightweight code index for any harness. This document exists
purely so the parity harness has some Markdown to chunk and render.

## Section one

The recursive splitter breaks this text into deterministic chunks. Because the
splitter lives in the shared Rust engine, Python and TypeScript must produce the
exact same chunk boundaries for the same input and configuration.

## Section two

Here is a second paragraph with enough words to push the splitter past a small
chunk size. Parity is about equal outputs, not shared caches. The two hosts each
keep their own memoization state under their own database path.

- bullet one
- bullet two
- bullet three

## Section three

A final closing paragraph so the file spans several chunks. The more content we
add here, the more chunk boundaries the two hosts have to agree on byte for byte.
