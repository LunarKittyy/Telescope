//! The textures kept for the next frame, by size. A few of each size, and the sizes nobody has asked for in a while are
//! dropped first once the lot is over its byte budget, so dragging the preview through many sizes doesn't keep them all.

use std::collections::HashMap;
use std::hash::Hash;

/// How many of one size are kept: a frame's steps want a few at once.
const PER_SIZE: usize = 4;
/// A size untouched for this many takes and gives is dropped, so a picture size nobody uses any more doesn't stay.
const IDLE_TICKS: u64 = 1200;

struct Sized<T> {
    items: Vec<(T, usize)>,
    // The tick of the last take or give, so the sizes in use stay and the ones that aren't go first
    used: u64,
}

pub struct Pool<K, T> {
    sizes: HashMap<K, Sized<T>>,
    tick: u64,
    bytes: usize,
    budget: usize,
}

impl<K: Hash + Eq + Copy, T> Pool<K, T> {
    pub fn new(budget: usize) -> Self {
        Pool { sizes: HashMap::new(), tick: 0, bytes: 0, budget }
    }

    pub fn take(&mut self, key: K) -> Option<T> {
        self.tick += 1;
        let size = self.sizes.get_mut(&key)?;
        size.used = self.tick;
        let (item, bytes) = size.items.pop()?;
        self.bytes -= bytes;
        Some(item)
    }

    /// Keeps item (taking `bytes` of memory) for key's next frame, if there's room for another of that size.
    pub fn give(&mut self, key: K, item: T, bytes: usize) {
        self.tick += 1;
        let size = self.sizes.entry(key).or_insert_with(|| Sized { items: Vec::new(), used: 0 });
        size.used = self.tick;
        if size.items.len() < PER_SIZE {
            size.items.push((item, bytes));
            self.bytes += bytes;
        }
        let tick = self.tick;
        let stale: Vec<K> = self.sizes.iter().filter(|(_, s)| s.used + IDLE_TICKS < tick).map(|(k, _)| *k).collect();
        for key in stale {
            if let Some(gone) = self.sizes.remove(&key) {
                self.bytes -= gone.items.iter().map(|(_, b)| b).sum::<usize>();
            }
        }
        while self.bytes > self.budget {
            let Some(oldest) = self.sizes.iter().min_by_key(|(_, s)| s.used).map(|(k, _)| *k) else { break };
            if let Some(gone) = self.sizes.remove(&oldest) {
                self.bytes -= gone.items.iter().map(|(_, b)| b).sum::<usize>();
            }
        }
    }

    /// Lets go of everything kept (the stream stopped, so nothing will ask for it soon).
    pub fn clear(&mut self) {
        self.sizes.clear();
        self.bytes = 0;
    }

    /// How many bytes are kept.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// How many are kept in all.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.sizes.values().map(|s| s.items.len()).sum()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A texture stand-in that counts how many are still alive.
    struct Tex(Arc<AtomicUsize>);

    impl Tex {
        fn new(alive: &Arc<AtomicUsize>) -> Tex {
            alive.fetch_add(1, Ordering::SeqCst);
            Tex(alive.clone())
        }
    }

    impl Drop for Tex {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_size_nobody_asks_for_any_more_is_let_go_after_a_while() {
        let alive = Arc::new(AtomicUsize::new(0));
        let mut pool = Pool::new(1 << 30);
        for _ in 0..4 {
            pool.give((3840, 2160), Tex::new(&alive), 33_000_000);
        }
        assert_eq!(pool.bytes(), 4 * 33_000_000);
        // the preview moved to a smaller size and works there
        for _ in 0..(IDLE_TICKS as usize) {
            let t = pool.take((1280, 720)).unwrap_or_else(|| Tex::new(&alive));
            pool.give((1280, 720), t, 3_600_000);
        }
        assert_eq!(pool.bytes(), 3_600_000);
        assert_eq!(alive.load(Ordering::SeqCst), 1);
        pool.clear();
        assert_eq!((pool.bytes(), alive.load(Ordering::SeqCst)), (0, 0));
    }

    #[test]
    fn a_size_keeps_a_few_and_hands_them_back() {
        let alive = Arc::new(AtomicUsize::new(0));
        let mut pool = Pool::new(1 << 30);
        for _ in 0..10 {
            pool.give((64, 36), Tex::new(&alive), 100);
        }
        assert_eq!(alive.load(Ordering::SeqCst), PER_SIZE);
        assert!(pool.take((64, 36)).is_some());
        assert!(pool.take((32, 18)).is_none());
        assert_eq!(pool.len(), PER_SIZE - 1);
    }

    #[test]
    fn sizes_nobody_uses_any_more_are_dropped_first_when_over_budget() {
        let alive = Arc::new(AtomicUsize::new(0));
        // Room for ten textures of 100 bytes
        let mut pool = Pool::new(1000);
        // A window dragged through 200 sizes while a 1080p stream carries on
        for i in 0..200u32 {
            pool.give((1920, 1080), Tex::new(&alive), 100);
            let t = pool.take((1920, 1080)).unwrap();
            pool.give((100 + i, 50), Tex::new(&alive), 100);
            pool.give((1920, 1080), t, 100);
            assert!(alive.load(Ordering::SeqCst) <= 10, "{} textures kept after {i} sizes", alive.load(Ordering::SeqCst));
        }
        // The size in use survived all that
        assert!(pool.take((1920, 1080)).is_some());
        // ...and the first size dragged through didn't
        assert!(pool.take((100, 50)).is_none());
        drop(pool);
        assert_eq!(alive.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn one_texture_over_the_whole_budget_is_not_kept() {
        let alive = Arc::new(AtomicUsize::new(0));
        let mut pool = Pool::new(10);
        pool.give(1, Tex::new(&alive), 50);
        assert!(pool.is_empty());
        assert_eq!(alive.load(Ordering::SeqCst), 0);
    }
}
