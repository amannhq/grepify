//! Fingerprinting bindings — the napi analogue of `grepify`'s content
//! fingerprints.
//!
//! JS values are canonicalized on the JS side (msgpack) and passed here as a
//! `Buffer`; we then fingerprint the raw bytes with the engine's Blake2-based
//! fingerprinter so IDs and memo keys stay consistent with the Rust/Python
//! hosts.
//!
//! NOTE: `fingerprintSimpleObject` (canonicalize an arbitrary JS object *in
//! Rust*, the port of `rust/py/src/memo_fingerprint.rs`) is intentionally left
//! to the TS layer for now — TS serializes to msgpack and calls
//! [`fingerprint_bytes`]. See the plan's Phase 1 notes.

use grepify_utils::fingerprint::Fingerprint;
use napi::bindgen_prelude::Buffer;
use napi_derive::napi;

/// Fingerprint raw (already-canonicalized, e.g. msgpack) bytes, returning the
/// base64 fingerprint string used across Grepify hosts.
#[napi]
pub fn fingerprint_bytes(data: Buffer) -> String {
    Fingerprint::from_bytes(&data).to_base64()
}
