#![allow(clippy::expect_used)]

use mavrouter_rs::router::EndpointId;
use mavrouter_rs::routing::RoutingTable;
use parking_lot::RwLock;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_concurrent_routing_updates_high_load() {
    let rt = Arc::new(RwLock::new(RoutingTable::new()));
    let mut handles = vec![];

    // 20 Writers: Heavy update load
    for i in 0..20 {
        let rt_clone = rt.clone();
        handles.push(thread::spawn(move || {
            for j in 0..1000 {
                let sys = ((j % 50) + 1) as u8;
                let comp = ((j % 20) + 1) as u8;
                let mut lock = rt_clone.write();
                lock.update(EndpointId(i), sys, comp);
                // Simulate varying workloads
                if j % 100 == 0 {
                    drop(lock);
                    thread::sleep(Duration::from_micros(10));
                }
            }
        }));
    }

    // 20 Readers: Heavy read load
    for i in 0..20 {
        let rt_clone = rt.clone();
        handles.push(thread::spawn(move || {
            for j in 0..1000 {
                let sys = ((j % 50) + 1) as u8;
                let comp = ((j % 20) + 1) as u8;
                let lock = rt_clone.read();
                let _ = lock.should_send(EndpointId(i), sys, comp);
                if j % 100 == 0 {
                    drop(lock);
                    thread::sleep(Duration::from_micros(10));
                }
            }
        }));
    }

    // 1 Pruner
    let rt_clone = rt.clone();
    handles.push(thread::spawn(move || {
        for _ in 0..10 {
            thread::sleep(Duration::from_millis(5));
            let mut lock = rt_clone.write();
            lock.prune(Duration::from_millis(100));
        }
    }));

    for handle in handles {
        handle.join().expect("Thread join failed");
    }

    // Verification
    let stats = rt.read().stats();
    println!("Concurrent Test Stats: {:?}", stats);
    // We don't assert exact numbers because of pruning race, but we ensure no deadlocks/panics
    // and that the table isn't corrupt (accessing stats works).
}
