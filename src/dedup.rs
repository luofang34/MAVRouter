use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

pub struct Dedup {
    dedup_period: Duration,
    history: VecDeque<(Instant, u64)>,
    set: HashSet<u64>,
}

impl Dedup {
    pub fn new(dedup_period: Duration) -> Self {
        Self {
            dedup_period,
            history: VecDeque::new(),
            set: HashSet::new(),
        }
    }

    pub fn is_duplicate(&mut self, payload: &[u8]) -> bool {
        if self.dedup_period.is_zero() {
            return false;
        }

        let now = Instant::now();

        // Cleanup old entries
        while let Some((timestamp, hash)) = self.history.front() {
            if now.duration_since(*timestamp) > self.dedup_period {
                self.set.remove(hash);
                self.history.pop_front();
            } else {
                break;
            }
        }

        // Calculate hash (using simple hasher for speed, standard Hash trait)
        // C++ uses std::hash<std::string> on the buffer.
        // We can use DefaultHasher.
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        payload.hash(&mut hasher);
        let hash = hasher.finish();

        if self.set.contains(&hash) {
            true
        } else {
            self.set.insert(hash);
            self.history.push_back((now, hash));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup() {
        let mut dedup = Dedup::new(Duration::from_millis(100));
        let data = b"hello";

        assert!(!dedup.is_duplicate(data));
        assert!(dedup.is_duplicate(data));

        std::thread::sleep(Duration::from_millis(110));
        assert!(!dedup.is_duplicate(data));
    }
}
