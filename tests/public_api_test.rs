//! Golden public-API file.
//!
//! This test asserts the **minimal** shape of `mavrouter`'s public surface
//! by only using the symbols the library is contracted to expose. If a
//! future refactor renames or removes one of these items, this test will
//! fail to compile — the CI red light is the point.
//!
//! Symmetrically, if a refactor accidentally re-exposes an internal module
//! (say, `mavrouter::orchestration`), this file will still compile cleanly
//! — but the `cargo public-api` diff CI job will flag the widening. The
//! two guardrails are complementary: this file keeps the intended surface
//! honest; the API-diff CI keeps *unintended* additions honest.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use mavrouter::config::{Config, EndpointConfig};
use mavrouter::error::{Result, RouterError};
use mavrouter::Router;

/// Compile-time proof that each item in the intended public surface is
/// reachable. The test body doesn't need to *run* anything clever — if
/// this function compiles, the signatures are still there.
#[allow(dead_code)]
fn public_api_symbols() {
    // Error surface.
    let _err: RouterError = RouterError::config("example");
    let _res: Result<()> = Ok(());

    // Config surface — parse (and a glimpse at `EndpointConfig` as a
    // publicly-reachable enum).
    let _cfg: std::result::Result<Config, RouterError> = Config::parse("");
    let _ep: Option<EndpointConfig> = None;
}

/// Smoke test: start and stop the router through its documented entry points.
#[tokio::test]
async fn public_api_smoke_router_from_str_start_stop() {
    let router = Router::from_str(
        r#"
[general]
bus_capacity = 16

[[endpoint]]
type = "udp"
address = "127.0.0.1:0"
mode = "server"
"#,
    )
    .await
    .expect("router should start");

    assert!(
        router.is_running(),
        "freshly started router should be running"
    );

    // Subscribe to the message bus — demonstrates `Router::bus()` is on
    // the public surface and returns a type users can call `.subscribe()`
    // on without naming any `pub(crate)` intermediate.
    let _subscriber = router.bus().subscribe();

    let token = router.cancel_token();
    assert!(!token.is_cancelled());

    router.stop().await;
    assert!(token.is_cancelled());
}
