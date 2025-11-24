//! Safe lock helpers that handle poisoning gracefully
//! 
//! parking_lot locks never poison, but we keep these wrappers
//! for consistency and potential future migration to std locks.

/// Safely acquire a parking_lot::Mutex lock
#[macro_export]
macro_rules! lock_mutex {
    ($mutex:expr) => {{
        // parking_lot::Mutex::lock() never panics
        $mutex.lock()
    }};
}

/// Safely acquire a parking_lot::RwLock read lock
#[macro_export]
macro_rules! lock_read {
    ($rwlock:expr) => {{
        $rwlock.read()
    }};
}

/// Safely acquire a parking_lot::RwLock write lock
#[macro_export]
macro_rules! lock_write {
    ($rwlock:expr) => {{
        $rwlock.write()
    }};
}
