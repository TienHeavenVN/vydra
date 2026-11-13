//! Cache contract and implementations.
//!
//! Three cache classes exist (§4 of `docs/research/ARCHITECTURE_COMPARISON.md`, ADR-010):
//! session/bootstrap state, metadata and (opt-in) search pages. Failures are never cached, and every
//! entry records the provider version that produced it.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Cache namespace. Separating classes prevents a short-lived search page from evicting bootstrap
/// state that costs a round-trip to rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheClass {
    Session,
    Bootstrap,
    Metadata,
    Search,
}

impl CacheClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Bootstrap => "bootstrap",
            Self::Metadata => "metadata",
            Self::Search => "search",
        }
    }
}

/// A cached value with its freshness metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub value: String,
    pub inserted: Instant,
    pub ttl: Duration,
    /// Hash of the provider configuration that produced the value; a mismatch invalidates it.
    pub config_hash: u64,
}

impl CacheEntry {
    pub fn is_fresh(&self, now: Instant) -> bool {
        now.duration_since(self.inserted) < self.ttl
    }

    pub fn is_stale_usable(&self, now: Instant, stale_window: Duration) -> bool {
        now.duration_since(self.inserted) < self.ttl + stale_window
    }
}

/// Outcome of a lookup, distinguishing fresh from stale-while-revalidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Miss,
    Fresh(String),
    Stale(String),
}

/// Cache contract. Implementations must be safe to call from many tasks.
pub trait Cache: Send + Sync {
    fn get(&self, class: CacheClass, key: &str, config_hash: u64) -> Lookup;
    fn set(&self, class: CacheClass, key: &str, value: String, ttl: Duration, config_hash: u64);
    fn invalidate(&self, class: CacheClass, key: &str);
    fn invalidate_class(&self, class: CacheClass);
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
struct Slot {
    entry: CacheEntry,
    stale_window: Duration,
}

/// Bounded in-process cache with LRU-ish eviction (oldest insertion first).
#[derive(Debug)]
pub struct MemoryCache {
    max_entries: usize,
    slots: Mutex<HashMap<(CacheClass, String), Slot>>,
}

impl MemoryCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            slots: Mutex::new(HashMap::new()),
        }
    }

    fn evict_if_needed(slots: &mut HashMap<(CacheClass, String), Slot>, max_entries: usize) {
        if slots.len() <= max_entries {
            return;
        }
        let mut oldest: Option<((CacheClass, String), Instant)> = None;
        for (key, slot) in slots.iter() {
            if oldest
                .as_ref()
                .is_none_or(|(_, at)| slot.entry.inserted < *at)
            {
                oldest = Some((key.clone(), slot.entry.inserted));
            }
        }
        if let Some((key, _)) = oldest {
            slots.remove(&key);
        }
    }
}

impl Cache for MemoryCache {
    fn get(&self, class: CacheClass, key: &str, config_hash: u64) -> Lookup {
        let now = Instant::now();
        let Ok(slots) = self.slots.lock() else {
            return Lookup::Miss;
        };
        let Some(slot) = slots.get(&(class, key.to_owned())) else {
            return Lookup::Miss;
        };
        if slot.entry.config_hash != config_hash {
            return Lookup::Miss;
        }
        if slot.entry.is_fresh(now) {
            Lookup::Fresh(slot.entry.value.clone())
        } else if slot.entry.is_stale_usable(now, slot.stale_window) {
            Lookup::Stale(slot.entry.value.clone())
        } else {
            Lookup::Miss
        }
    }

    fn set(&self, class: CacheClass, key: &str, value: String, ttl: Duration, config_hash: u64) {
        let Ok(mut slots) = self.slots.lock() else {
            return;
        };
        slots.insert(
            (class, key.to_owned()),
            Slot {
                entry: CacheEntry {
                    value,
                    inserted: Instant::now(),
                    ttl,
                    config_hash,
                },
                stale_window: Duration::ZERO,
            },
        );
        Self::evict_if_needed(&mut slots, self.max_entries);
    }

    fn invalidate(&self, class: CacheClass, key: &str) {
        if let Ok(mut slots) = self.slots.lock() {
            slots.remove(&(class, key.to_owned()));
        }
    }

    fn invalidate_class(&self, class: CacheClass) {
        if let Ok(mut slots) = self.slots.lock() {
            slots.retain(|(slot_class, _), _| *slot_class != class);
        }
    }

    fn len(&self) -> usize {
        self.slots.lock().map(|slots| slots.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_entries_are_returned() {
        let cache = MemoryCache::new(4);
        cache.set(
            CacheClass::Metadata,
            "v:1",
            "{\"a\":1}".into(),
            Duration::from_secs(60),
            7,
        );
        assert_eq!(
            cache.get(CacheClass::Metadata, "v:1", 7),
            Lookup::Fresh("{\"a\":1}".into())
        );
    }

    #[test]
    fn config_hash_change_invalidates() {
        let cache = MemoryCache::new(4);
        cache.set(
            CacheClass::Metadata,
            "k",
            "v".into(),
            Duration::from_secs(60),
            1,
        );
        assert_eq!(cache.get(CacheClass::Metadata, "k", 2), Lookup::Miss);
    }

    #[test]
    fn expiry_is_respected() {
        let cache = MemoryCache::new(4);
        cache.set(CacheClass::Search, "k", "v".into(), Duration::ZERO, 0);
        assert_eq!(cache.get(CacheClass::Search, "k", 0), Lookup::Miss);
    }

    #[test]
    fn capacity_is_bounded() {
        let cache = MemoryCache::new(2);
        for index in 0..5 {
            cache.set(
                CacheClass::Metadata,
                &format!("k{index}"),
                "v".into(),
                Duration::from_secs(60),
                0,
            );
        }
        assert!(cache.len() <= 2);
    }

    #[test]
    fn class_invalidation_only_clears_that_class() {
        let cache = MemoryCache::new(8);
        cache.set(
            CacheClass::Metadata,
            "a",
            "v".into(),
            Duration::from_secs(60),
            0,
        );
        cache.set(
            CacheClass::Session,
            "b",
            "v".into(),
            Duration::from_secs(60),
            0,
        );
        cache.invalidate_class(CacheClass::Metadata);
        assert_eq!(cache.get(CacheClass::Metadata, "a", 0), Lookup::Miss);
        assert!(matches!(
            cache.get(CacheClass::Session, "b", 0),
            Lookup::Fresh(_)
        ));
    }
}
