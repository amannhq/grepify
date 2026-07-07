// msgpack (de)serialization for values crossing the JS <-> Rust boundary.
//
// Unlike the Python host (which supports pickle), the TS host is msgpack-only:
// every value stored by a component, passed as a memo key input, or returned
// from `App.update` is msgpack-encoded here and decoded on the way back.

import { decode as msgpackDecode, encode as msgpackEncode } from '@msgpack/msgpack'

/** msgpack-encode an arbitrary JS value into a Node `Buffer`. */
export function encode(value: unknown): Buffer {
  const bytes = msgpackEncode(value)
  return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength)
}

/** msgpack-decode bytes produced by {@link encode} back into a JS value. */
export function decode<T = unknown>(buf: Buffer | Uint8Array): T {
  return msgpackDecode(buf) as T
}
