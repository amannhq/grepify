//! Rust-native side of the binding benchmarks — the zero-boundary baseline.
//!
//! Measures the same workload as the Python (PyO3) and TypeScript (napi)
//! drivers, but calling the `grepify` SDK directly (no FFI). The Python/TS
//! numbers minus these numbers approximate the host-binding overhead.
//!
//!   * Boundary-crossing overhead (here: raw engine cost, no boundary):
//!       - small sync call: `detect_code_language`
//!       - heavier sync call: `match_code`
//!       - async mount round-trip: N sequential `ctx.scope` child mounts
//!   * Pipeline: `mount_each` over a generated corpus, cold vs warm (memo-hit).
//!
//! Writes a metrics JSON consumed by ../runner.py.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use grepify::fs::walk;
use grepify::ops::code::match_code;
use grepify::ops::text::{detect_code_language, RecursiveChunkConfig, RecursiveSplitter};
use grepify::{Ctx, Environment};
use serde_json::{json, Value};

const MATCH_SRC: &str = "def foo(a, b):\n    return a + b\n\ndef bar(x):\n    return x\n";
const MATCH_PATTERN: &str = r"def \NAME(\(ARGS*\)):";
const CHUNK_SIZE: usize = 256;
const LANGUAGE: &str = "markdown";

static SPLITTER: OnceLock<RecursiveSplitter> = OnceLock::new();

fn splitter() -> &'static RecursiveSplitter {
    SPLITTER.get_or_init(|| RecursiveSplitter::new().expect("build splitter"))
}

struct Args {
    corpus: PathBuf,
    db: PathBuf,
    metrics: PathBuf,
    detect_iters: u64,
    match_iters: u64,
    mount_iters: u64,
}

fn parse_args() -> Args {
    let mut corpus = None;
    let mut db = None;
    let mut metrics = None;
    let mut detect_iters = 500_000u64;
    let mut match_iters = 50_000u64;
    let mut mount_iters = 2_000u64;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let val = it.next().unwrap_or_default();
        match flag.as_str() {
            "--corpus" => corpus = Some(PathBuf::from(val)),
            "--db" => db = Some(PathBuf::from(val)),
            "--metrics" => metrics = Some(PathBuf::from(val)),
            "--detect-iters" => detect_iters = val.parse().unwrap(),
            "--match-iters" => match_iters = val.parse().unwrap(),
            "--mount-iters" => mount_iters = val.parse().unwrap(),
            other => panic!("unknown flag: {other}"),
        }
    }

    Args {
        corpus: corpus.expect("--corpus required"),
        db: db.expect("--db required"),
        metrics: metrics.expect("--metrics required"),
        detect_iters,
        match_iters,
        mount_iters,
    }
}

#[grepify::function(memo)]
async fn process_file(_ctx: &Ctx, abspath: &String) -> grepify::Result<usize> {
    // Memo key is derived from `abspath` (+ logic hash); on a warm re-run this
    // whole function is a memo hit, so the read + split is skipped.
    let text = std::fs::read_to_string(abspath).map_err(grepify::Error::Io)?;
    let chunks = splitter().split_with(
        &text,
        RecursiveChunkConfig {
            chunk_size: CHUNK_SIZE,
            min_chunk_size: None,
            chunk_overlap: None,
            language: Some(LANGUAGE.to_string()),
        },
    );
    Ok(chunks.len())
}

fn bench_boundary_sync(detect_iters: u64, match_iters: u64) -> grepify::Result<Value> {
    let _ = detect_code_language("main.py");
    let t0 = Instant::now();
    for _ in 0..detect_iters {
        let _ = detect_code_language("main.py");
    }
    let detect_ns = t0.elapsed().as_secs_f64() / detect_iters as f64 * 1e9;

    let _ = match_code(MATCH_PATTERN, MATCH_SRC, "python")?;
    let t0 = Instant::now();
    for _ in 0..match_iters {
        let _ = match_code(MATCH_PATTERN, MATCH_SRC, "python")?;
    }
    let match_ns = t0.elapsed().as_secs_f64() / match_iters as f64 * 1e9;

    Ok(json!({
        "detect_ns_per_call": (detect_ns * 10.0).round() / 10.0,
        "match_ns_per_call": (match_ns * 10.0).round() / 10.0,
        "detect_iters": detect_iters,
        "match_iters": match_iters,
    }))
}

fn run_stats_json(stats: &grepify::RunStats) -> Value {
    json!({
        "num_adds": stats.processed,
        "num_reprocesses": 0,
        "num_unchanged": stats.skipped,
        "num_deletes": stats.deleted,
        "written": stats.written,
    })
}

#[tokio::main]
async fn main() -> grepify::Result<()> {
    let args = parse_args();

    let mut result = json!({ "host": "rust" });
    result["boundary_sync"] = bench_boundary_sync(args.detect_iters, args.match_iters)?;

    // Async mount round-trip. Measured inside the run closure to exclude setup.
    let rt_env = Environment::builder()
        .db_path(format!("{}_rt", args.db.display()))
        .build()
        .await?;
    let rt_app = rt_env.app("bench_roundtrip").await?;
    let rt_elapsed = Arc::new(Mutex::new(0f64));
    let rt_ref = rt_elapsed.clone();
    let mount_iters = args.mount_iters;
    rt_app
        .run(move |ctx| async move {
            let t0 = Instant::now();
            for i in 0..mount_iters {
                ctx.scope(&format!("rt/{i}"), |_c| async {
                    Ok::<i32, grepify::Error>(0)
                })
                .await?;
            }
            *rt_ref.lock().unwrap() = t0.elapsed().as_secs_f64();
            Ok::<(), grepify::Error>(())
        })
        .await?;
    let mount_us = *rt_elapsed.lock().unwrap() / mount_iters as f64 * 1e6;
    result["boundary_async"] = json!({
        "mount_us_per_op": (mount_us * 1000.0).round() / 1000.0,
        "mount_iters": mount_iters,
    });

    // Pipeline: cold vs warm.
    let env = Environment::builder()
        .db_path(format!("{}_pipe", args.db.display()))
        .build()
        .await?;
    let app = env.app("bench_pipeline").await?;

    let n_files = walk(&args.corpus, &["**/*.md"])?.len();

    let corpus_cold = args.corpus.clone();
    let cold = app
        .run(move |ctx| async move {
            let files = walk(&corpus_cold, &["**/*.md"])?;
            let items: Vec<(String, String)> = files
                .iter()
                .map(|f| (f.key(), f.path().to_string_lossy().to_string()))
                .collect();
            let counts: Vec<usize> = ctx
                .mount_each(
                    items,
                    |item| item.0.clone(),
                    |cctx, item| async move { process_file(&cctx, &item.1).await },
                )
                .await?;
            Ok::<usize, grepify::Error>(counts.iter().sum())
        })
        .await?;

    let corpus_warm = args.corpus.clone();
    let warm = app
        .run(move |ctx| async move {
            let files = walk(&corpus_warm, &["**/*.md"])?;
            let items: Vec<(String, String)> = files
                .iter()
                .map(|f| (f.key(), f.path().to_string_lossy().to_string()))
                .collect();
            let counts: Vec<usize> = ctx
                .mount_each(
                    items,
                    |item| item.0.clone(),
                    |cctx, item| async move { process_file(&cctx, &item.1).await },
                )
                .await?;
            Ok::<usize, grepify::Error>(counts.iter().sum())
        })
        .await?;

    result["pipeline"] = json!({
        "corpus_files": n_files,
        "cold_ms": (cold.elapsed.as_secs_f64() * 1000.0 * 100.0).round() / 100.0,
        "warm_ms": (warm.elapsed.as_secs_f64() * 1000.0 * 100.0).round() / 100.0,
        "cold_stats": run_stats_json(&cold),
        "warm_stats": run_stats_json(&warm),
    });

    if let Some(parent) = args.metrics.parent() {
        std::fs::create_dir_all(parent).map_err(grepify::Error::Io)?;
    }
    std::fs::write(
        &args.metrics,
        serde_json::to_vec_pretty(&result).map_err(|e| grepify::Error::engine(e.to_string()))?,
    )
    .map_err(grepify::Error::Io)?;

    Ok(())
}
