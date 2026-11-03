//! Timbre — the connector for Apple Mail and Claude.
//!
//! Copyright (c) 2026 Alejandro San Martin Lamas — https://github.com/alejandrosml
//! Licensed under the MIT License.

mod actions;
mod app;
mod config;
#[cfg(debug_assertions)]
mod corpus;
mod degraded;
mod emlx;
mod extract;
#[cfg(any(test, debug_assertions))]
mod fixture;
mod html;
mod index;
mod mailapp;
mod mime;
mod norm;
mod quote;
mod search;
mod security;
mod server;
mod store;
#[cfg(test)]
mod tests;
mod util;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

use crate::config::Config;

// Gives the binary a stable identity and the text macOS shows when Timbre
// asks to control Mail.
#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../Info.plist");

#[derive(Parser)]
#[command(name = "timbre", version, about = "Timbre — the connector for Apple Mail and Claude")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server on standard input/output (the default).
    Serve,
    /// Build or update the full-text index, with progress.
    Index {
        /// Rebuild the index from scratch.
        #[arg(long)]
        rebuild: bool,
    },
    /// Search from the terminal (same syntax as the search_mail tool).
    Search {
        query: Vec<String>,
        #[arg(short, long, default_value_t = 10)]
        limit: u32,
        /// Print the raw JSON result.
        #[arg(long)]
        json: bool,
    },
    /// Check access to Mail's data, the schema and the index.
    Doctor {
        /// Also check that macOS lets Timbre control Mail (read-only probe).
        #[arg(long)]
        automation: bool,
    },
    /// Write a synthetic Mail store for tests (debug builds only).
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    DevFixture { dir: std::path::PathBuf },
    /// Build a large synthetic store for benchmarks.
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    DevCorpus {
        dir: std::path::PathBuf,
        #[arg(default_value_t = 20_000)]
        count: usize,
    },
    /// Measure search latency on this Mac.
    Bench {
        #[arg(long, default_value_t = 30)]
        runs: usize,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(EnvFilter::try_from_env("TIMBRE_LOG").unwrap_or_else(|_| EnvFilter::new("warn")))
        .init();
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::capture();
        let bt =
            if bt.status() == std::backtrace::BacktraceStatus::Captured { format!("\n{bt}") } else { String::new() };
        tracing::error!("panic in thread {}: {info}{bt}", std::thread::current().name().unwrap_or("?"));
    }));
    let cli = Cli::parse();
    let cfg = Config::from_env();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(cfg),
        Command::Index { rebuild } => run_index(cfg, rebuild),
        Command::Search { query, limit, json } => run_search(cfg, query.join(" "), limit, json),
        Command::Doctor { automation } => doctor(cfg, automation),
        #[cfg(debug_assertions)]
        Command::DevFixture { dir } => fixture::build(&dir).map(|_| ()),
        #[cfg(debug_assertions)]
        Command::DevCorpus { dir, count } => corpus::build(&dir, count).map(|_| ()),
        Command::Bench { runs } => bench(cfg, runs),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?)
}

fn serve(cfg: Config) -> Result<()> {
    let rt = runtime()?;
    let store = match store::Store::open(&cfg) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            // Still start, so Claude can explain what is missing.
            tracing::warn!("mail store unavailable: {e:#}");
            return rt.block_on(async move {
                let service = degraded::Degraded::new(format!("{e:#}")).serve(rmcp::transport::stdio()).await?;
                service.waiting().await?;
                Ok(())
            });
        }
    };
    drop(index::open(&cfg, &store, true)?);
    // Parsing threads for the index: utility priority, and roomy stacks for
    // hostile, deeply nested messages.
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(8 << 20)
        .start_handler(|_| index::background_priority())
        .build_global();
    let progress = Arc::new(index::Progress::default());
    let indexer = index::Indexer::new(cfg.clone(), store.clone(), progress.clone());
    std::thread::Builder::new().name("timbre-index".into()).spawn(move || indexer.run_forever())?;
    let ctx = Arc::new(app::Ctx::new(cfg, store, progress)?);
    let result = rt.block_on(async move {
        let service = server::Timbre::new(ctx).serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        anyhow::Ok(())
    });
    // Claude closed stdin: exit now rather than wait for a running query.
    rt.shutdown_timeout(Duration::from_secs(2));
    result
}

fn open_ctx(cfg: Config) -> Result<Arc<app::Ctx>> {
    let store = Arc::new(store::Store::open(&cfg)?);
    let conn = index::open(&cfg, &store, true)?;
    let progress = Arc::new(index::Progress::default());
    if index::get_meta(&conn, "built").is_some() {
        progress.phase.store(index::PHASE_READY, Ordering::Relaxed);
    }
    drop(conn);
    Ok(Arc::new(app::Ctx::new(cfg, store, progress)?))
}

fn run_index(cfg: Config, rebuild: bool) -> Result<()> {
    let store = Arc::new(store::Store::open(&cfg)?);
    let mut conn = index::open(&cfg, &store, true)?;
    let progress = Arc::new(index::Progress::default());
    let p2 = progress.clone();
    let reporter = std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            let phase = p2.phase.load(Ordering::Relaxed);
            if phase == index::PHASE_READY || phase == index::PHASE_FAILED {
                break;
            }
            let (d, t) = (p2.done.load(Ordering::Relaxed), p2.total.load(Ordering::Relaxed));
            if phase == index::PHASE_INDEXING && t > 0 {
                eprint!("\rindexing {d}/{t} ({}%)   ", d * 100 / t);
            } else {
                eprint!("\rscanning Mail's store…   ");
            }
        }
    });
    let start = Instant::now();
    let mut indexer = index::Indexer::new(cfg.clone(), store.clone(), progress.clone());
    let res = indexer.full_build(&mut conn, rebuild);
    if res.is_err() {
        progress.phase.store(index::PHASE_FAILED, Ordering::Relaxed);
    }
    let _ = reporter.join();
    res?;
    let size = std::fs::metadata(cfg.index_path()).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "\rindex ready: {} messages in {:.1}s ({} MB)            ",
        progress.docs.load(Ordering::Relaxed),
        start.elapsed().as_secs_f64(),
        size / 1_048_576
    );
    Ok(())
}

#[allow(clippy::print_stdout)] // CLI output
fn run_search(cfg: Config, query: String, limit: u32, json: bool) -> Result<()> {
    let ctx = open_ctx(cfg)?;
    let params = search::SearchParams { query: Some(query), limit: Some(limit), ..Default::default() };
    let start = Instant::now();
    let res = {
        let conn = ctx.db();
        search::search(&conn, &ctx.store, &ctx.cfg, &ctx.progress, &params).map_err(anyhow::Error::msg)?
    };
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if json {
        println!("{}", serde_json::to_string_pretty(&res)?);
        return Ok(());
    }
    println!("{} result(s) via {} in {ms:.1} ms", res.total, res.engine);
    for h in &res.results {
        println!("\n#{}  {}  {}\n    {}\n    {}", h.id, h.date, h.mailbox, h.from, h.subject);
        if let Some(s) = &h.snippet {
            println!("    {s}");
        }
    }
    Ok(())
}

#[allow(clippy::print_stdout)] // CLI output
fn doctor(cfg: Config, automation: bool) -> Result<()> {
    println!("Timbre {}", env!("CARGO_PKG_VERSION"));
    println!("binary: {}", std::env::current_exe()?.display());
    match store::Store::open(&cfg) {
        Err(e) => {
            println!("mail store: NOT ACCESSIBLE\n  {e:#}");
            return Ok(());
        }
        Ok(store) => {
            println!("mail store: {} ({:?})", store.version_dir.display(), store.open_mode);
            println!(
                "schema: unread={} flagged={} conversation={}",
                store.schema.unread, store.schema.flagged, store.schema.conversation
            );
            let conn = index::open(&cfg, &store, true)?;
            let (total, unread) = store.stats(&conn)?;
            println!(
                "messages: {total} ({unread} unread), mailboxes: {}, accounts: {}",
                store.mailboxes().len(),
                store.account_names().join(", ")
            );
            let docs: i64 = conn.query_row("SELECT count(*) FROM docs WHERE status = 0", [], |r| r.get(0)).unwrap_or(0);
            println!(
                "index: {} ({} messages, built={})",
                cfg.index_path().display(),
                docs,
                index::get_meta(&conn, "built").is_some()
            );
            // How well the path prediction scheme matches this Mac.
            let mut stmt = conn.prepare("SELECT id, path FROM docs WHERE path != '' ORDER BY random() LIMIT 200")?;
            let rows: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect();
            if !rows.is_empty() {
                let ok = rows
                    .iter()
                    .filter(|(id, rel)| {
                        let p = store.version_dir.join(rel);
                        emlx::data_root(&p).and_then(|root| emlx::predict(&root, *id)).is_some_and(|l| l.path == p)
                    })
                    .count();
                println!("path prediction: {ok}/{} sampled messages", rows.len());
            }
            if automation {
                let inbox = store.mailboxes().values().find(|m| m.role == store::Role::Inbox).cloned();
                let latest: Option<i64> = inbox.as_ref().and_then(|mb| {
                    conn.query_row("SELECT max(ROWID) FROM mail.messages WHERE mailbox = ?1", [mb.id], |r| r.get(0))
                        .ok()
                        .flatten()
                });
                match (inbox, latest) {
                    (Some(mb), Some(id)) => {
                        let input = serde_json::json!({"loc": mailapp::Loc::from(&mb), "id": id});
                        match runtime()?.block_on(mailapp::run(mailapp::VERIFY, &input, Duration::from_secs(30))) {
                            Ok(v) => println!(
                                "automation: OK (Mail answered for message {id}: {})",
                                v.get("subject").and_then(|s| s.as_str()).unwrap_or("")
                            ),
                            Err(e) => println!("automation: FAILED — {e:#}"),
                        }
                    }
                    _ => println!("automation: no inbox message to probe"),
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::print_stdout)] // CLI output
fn bench(cfg: Config, runs: usize) -> Result<()> {
    let ctx = open_ctx(cfg)?;
    let queries =
        ["facture", "réunion", "invoice", "meeting", "rendez-vous", "contrat", "merci", "projet", "factura", "urgent"];
    let mut times = vec![];
    for i in 0..runs {
        let q = queries[i % queries.len()];
        let params = search::SearchParams { query: Some(q.to_string()), limit: Some(20), ..Default::default() };
        let start = Instant::now();
        let conn = ctx.db();
        let _ = search::search(&conn, &ctx.store, &ctx.cfg, &ctx.progress, &params);
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| times[((times.len() as f64 - 1.0) * p) as usize];
    println!("{} searches: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms", times.len(), pct(0.5), pct(0.95), pct(1.0));
    Ok(())
}
