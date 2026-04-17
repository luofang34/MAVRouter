//! The routing-table updater task: drains `RouteUpdate`s from an mpsc
//! channel and applies them under the single `routing_table.write()`
//! lock held anywhere in the crate.

use super::{NamedTask, ROUTE_UPDATE_BATCH_SIZE};
use crate::routing::{RouteUpdate, RoutingTable};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

/// Spawn the single routing-table updater task.
///
/// This is the *only* code path in the crate that acquires
/// `routing_table.write()`. It drains the route-update mpsc channel in
/// batches (up to `ROUTE_UPDATE_BATCH_SIZE` per write-lock acquisition)
/// and also drives periodic prune cycles from the same task — meaning
/// readers only ever contend with this one updater, and there is no
/// scenario where an async hot path holds a blocking write lock.
///
/// The task exits on cancellation or when all update senders have been
/// dropped (e.g. all endpoints have finished during shutdown).
pub fn spawn_routing_updater(
    routing_table: Arc<RwLock<RoutingTable>>,
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
                    // Opportunistically drain more queued updates so we amortise
                    // the write-lock acquisition across many observations.
                    while batch.len() < ROUTE_UPDATE_BATCH_SIZE {
                        match update_rx.try_recv() {
                            Ok(u) => batch.push(u),
                            Err(_) => break,
                        }
                    }
                    let mut guard = routing_table.write();
                    for u in batch.iter() {
                        guard.update(u.endpoint_id, u.sys_id, u.comp_id, u.now);
                    }
                    drop(guard);
                }
                _ = prune_timer.tick() => {
                    let mut guard = routing_table.write();
                    guard.prune(prune_ttl);
                    drop(guard);
                }
            }
        }
    });
    NamedTask::new("Routing Updater", handle)
}
