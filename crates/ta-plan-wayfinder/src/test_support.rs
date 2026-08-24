// test_support.rs — test-only helper for running a `wiremock::MockServer`
// from plain synchronous `#[test]` functions.
//
// `reqwest::blocking::Client` builds its own internal tokio runtime and
// panics ("Cannot start a runtime from within a runtime") if called from
// inside an existing one — so tests exercising `WayfinderClient` (which is
// deliberately blocking, matching `PlanStore`'s own synchronicity) cannot
// be `#[tokio::test]` functions. Instead, the mock server runs on a
// dedicated background thread with its own current-thread runtime, and
// every test body stays a plain synchronous `#[test]`.

use wiremock::MockServer;

/// A `wiremock::MockServer` running on a background thread, reachable via
/// `.uri()` from synchronous test code. Registering mocks against it still
/// requires `.await` (wiremock's own API), so callers drive that through
/// `self.block_on(...)`.
pub struct BlockingMockServer {
    uri: String,
    runtime: tokio::runtime::Runtime,
    server: MockServer,
}

impl BlockingMockServer {
    pub fn start() -> Self {
        // A dedicated current-thread runtime, not the process-wide one —
        // this helper is called from plain `#[test]` functions with no
        // ambient runtime at all, so one must be created here.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build a runtime for the mock Wayfinder server");
        let server = runtime.block_on(MockServer::start());
        let uri = server.uri();
        Self {
            uri,
            runtime,
            server,
        }
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Registers a mock, or runs any other wiremock future, synchronously.
    pub fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.runtime.block_on(fut)
    }

    pub fn server(&self) -> &MockServer {
        &self.server
    }
}
