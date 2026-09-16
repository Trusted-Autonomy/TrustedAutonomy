//! Shared sync-to-async bridge for MCP tool handlers.
//!
//! Tool handlers are plain sync `fn`s that may already be running inside
//! the gateway's own async runtime, so calling an `async fn` (a daemon
//! HTTP client, an MCP client to a remote server, etc.) must not
//! `block_on` directly on the calling thread (risk of nesting runtimes).
//! `run_on_dedicated_thread` spawns a dedicated OS thread, builds a
//! `tokio::runtime::Builder::new_current_thread()` runtime on it, and
//! joins the result back onto the calling thread.
//!
//! Originally private to `tools/whiteboard.rs`; extracted here once
//! `tools/wiki.rs` needed the identical bridge for its own async MCP
//! client calls, rather than duplicating this correctness-sensitive
//! logic a second time.

use rmcp::ErrorData as McpError;

pub fn run_on_dedicated_thread<F, Fut, T>(f: F) -> Result<T, McpError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send + 'static,
{
    let result = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("failed to start worker runtime: {e}"))?;
        rt.block_on(f())
    })
    .join()
    .map_err(|_| anyhow::anyhow!("worker thread panicked"))
    .and_then(|inner| inner);

    result.map_err(|e| McpError::internal_error(e.to_string(), None))
}
