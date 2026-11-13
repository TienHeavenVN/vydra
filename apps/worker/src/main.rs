//! Vydra maintenance worker.
//!
//! Phase 8 scope: cache warming and quota snapshotting. For now the worker is a supervised loop with
//! explicit, observable ticks so that the deployment shape (one replica, Postgres lease) exists and
//! can be validated before any background behaviour is added.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used, clippy::panic))]

use std::time::Duration;

use vydra_config::Config;
use vydra_observability::init_tracing;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    init_tracing(&config.observability)?;
    tracing::info!(config = %config.redacted_summary(), "starting vydra worker");

    let interval = Duration::from_secs(60);
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // Deliberately does nothing yet: no background job is implemented, and fabricating
                // activity would misrepresent the project state (see docs/PROGRESS.md).
                tracing::debug!("worker tick: no jobs registered");
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("worker shutdown requested");
                break;
            }
        }
    }
    Ok(())
}
