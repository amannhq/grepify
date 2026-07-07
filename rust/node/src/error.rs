//! Error conversion helpers bridging Grepify SDK errors to `napi::Error`.

use grepify::error::Error as SdkError;

/// Convert any `Display` error into a `napi::Error` with a `GenericFailure`
/// status. Used throughout the bindings so JS callers see a normal `Error`.
pub(crate) fn to_napi<E: std::fmt::Display>(err: E) -> napi::Error {
    napi::Error::from_reason(err.to_string())
}

/// Convenience alias: map a `grepify::Result` into a `napi::Result`.
pub(crate) trait IntoNapiResult<T> {
    fn into_napi(self) -> napi::Result<T>;
}

impl<T> IntoNapiResult<T> for Result<T, SdkError> {
    fn into_napi(self) -> napi::Result<T> {
        self.map_err(to_napi)
    }
}

impl<T> IntoNapiResult<T> for Result<T, grepify_utils::error::Error> {
    fn into_napi(self) -> napi::Result<T> {
        self.map_err(to_napi)
    }
}
