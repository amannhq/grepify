//! Token-bucket rate limiter binding — the napi analogue of
//! `grepify::RateLimiter` (mirrors `rust/py/src/ratelimit.rs`).

use std::time::Duration;

use grepify::RateLimiter;
use napi_derive::napi;

use crate::error::IntoNapiResult;

/// A token-bucket rate limiter. `acquire(n)` returns a Promise that resolves
/// once `n` tokens are available; concurrent callers are served FIFO.
#[napi(js_name = "RateLimiter")]
pub struct RateLimiterJs {
    inner: RateLimiter,
}

#[napi]
impl RateLimiterJs {
    /// Create a limiter allowing `maxRowsPerSecond`, with burst capacity sized
    /// to `burstWindowSecs` (default 1s, matching the Python default).
    #[napi(constructor)]
    pub fn new(max_rows_per_second: f64, burst_window_secs: Option<f64>) -> napi::Result<Self> {
        let burst = Duration::from_secs_f64(burst_window_secs.unwrap_or(1.0).max(0.0));
        let inner = RateLimiter::new(max_rows_per_second, burst).into_napi()?;
        Ok(Self { inner })
    }

    /// Wait until `n` tokens are available (default 1).
    #[napi]
    pub async fn acquire(&self, n: Option<u32>) -> napi::Result<()> {
        self.inner.acquire(n.unwrap_or(1)).await.into_napi()
    }
}
