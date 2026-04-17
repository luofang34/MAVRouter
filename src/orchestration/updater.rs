//! The routing-table updater task.
//!
//! Drains `RouteUpdate`s from an mpsc channel and applies them to the
//! sharded `RoutingTable`. Each `update()` and `prune()` call takes the
//! matching shard's `RwLock` internally; batching is still useful to
//! amortise channel wake-ups, but no outer table-wide lock exists.

use super::{NamedTask, ROUTE_UPDATE_BATCH_SIZE};
use crate::routing::{RouteUpdate, RoutingTable};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

/// Spawn the single routing-table updater task.
///
/// Batches of `RouteUpdate`s are drained from the mpsc and applied via
/// `RoutingTable::update`; periodic prunes run from the same task through
/// `RoutingTable::prune`. The routing table is internally sharded, so
/// writes to one shard don't stall readers on the other shards.
///
/// The task exits on cancellation or when all update senders have been
/// dropped (e.g. all endpoints have finished during shutdown).
pub fn spawn_routing_updater(
    routing_table: Arc<RoutingTable>,
    mut update_rx: mpsc::Receiver<RouteUpdate>,
    prune_ttl: Duration,
    prune_interval: Duration,
    cancel: CancellationToken,
) -> NamedTask {
    let handle = tokio::spawn(async move {
        let mut prune_timer = tokio::time::interval(prune_interval);
        // The first tick fires immediately — skip it so we don't prune an
        // empty table the instant the router starts.
        prune_timer.tick().await;

        let mut batch: Vec<RouteUpdate> = Vec::with_capacity(ROUTE_UPDATE_BATCH_SIZE);

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    info!("Routing Updater shutting down.");
                    break;
                }
                maybe_update = update_rx.recv() => {
                    let Some(first) = maybe_update else {
                        debug!("Routing Updater: all update senders dropped, exiting.");
                        break;
                    };
                    batch.clear();
                    batch.push(first);
                    // Opportunistically drain more queued updates so we
                    // amortise the mpsc wake-up cost across many observations.
                    while batch.len() < ROUTE_UPDATE_BATCH_SIZE {
                        match update_rx.try_recv() {
                            Ok(u) => batch.push(u),
                            Err(_) => break,
                        }
                    }
                    for u in batch.iter() {
                        routing_table.update(u.endpoint_id, u.sys_id, u.comp_id, u.now);
                    }
                }
                _ = prune_timer.tick() => {
                    routing_table.prune(prune_ttl);
                }
            }
        }
    });
    NamedTask::new("Routing Updater", handle)
}
