//! Bounded LRU GPU texture tracker for 180x248 posters and 640x360 backdrops.

use std::collections::{HashMap, HashSet};

pub const DEFAULT_POSTER_CAP: usize = 48;
pub const DEFAULT_BACKDROP_CAP: usize = 1;

/// Tracks loaded and in-flight textures on the UI thread to keep VRAM < 20 MiB.
#[derive(Debug)]
pub struct TextureCacheTracker {
    poster_cap: usize,
    backdrop_cap: usize,
    tick: u64,
    loaded_posters: HashMap<String, u64>,
    in_flight_posters: HashSet<String>,
    loaded_backdrops: HashMap<String, u64>,
    in_flight_backdrop: Option<String>,
}

impl Default for TextureCacheTracker {
    fn default() -> Self {
        Self::new(DEFAULT_POSTER_CAP, DEFAULT_BACKDROP_CAP)
    }
}

impl TextureCacheTracker {
    pub fn new(poster_cap: usize, backdrop_cap: usize) -> Self {
        Self {
            poster_cap: poster_cap.max(1),
            backdrop_cap: backdrop_cap.max(1),
            tick: 0,
            loaded_posters: HashMap::new(),
            in_flight_posters: HashSet::new(),
            loaded_backdrops: HashMap::new(),
            in_flight_backdrop: None,
        }
    }

    pub fn loaded_poster_count(&self) -> usize {
        self.loaded_posters.len()
    }

    pub fn is_poster_loaded(&self, title_id: &str) -> bool {
        self.loaded_posters.contains_key(title_id)
    }

    pub fn is_backdrop_loaded(&self, title_id: &str) -> bool {
        self.loaded_backdrops.contains_key(title_id)
    }

    /// Touches visible + prefetch poster IDs, returns `(to_request, to_evict)`.
    pub fn visible_window_plan(
        &mut self,
        visible_ids: &[String],
        prefetch_ids: &[String],
    ) -> (Vec<String>, Vec<String>) {
        self.tick += 1;
        let now = self.tick;

        let mut keep_set: HashSet<&str> = HashSet::new();
        let mut ordered: Vec<&str> = Vec::with_capacity(visible_ids.len() + prefetch_ids.len());
        for id in visible_ids.iter().chain(prefetch_ids.iter()) {
            let key = id.as_str();
            if keep_set.insert(key) {
                ordered.push(key);
            }
        }

        let mut to_request = Vec::new();
        for key in ordered {
            if let Some(last_used) = self.loaded_posters.get_mut(key) {
                *last_used = now;
            } else if !self.in_flight_posters.contains(key) {
                self.in_flight_posters.insert(String::from(key));
                to_request.push(String::from(key));
            }
        }

        let mut to_evict = Vec::new();
        if self.loaded_posters.len() > self.poster_cap {
            let mut candidates: Vec<(u64, String)> = self
                .loaded_posters
                .iter()
                .filter(|(k, _)| !keep_set.contains(k.as_str()))
                .map(|(k, &ts)| (ts, k.clone()))
                .collect();
            candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

            let mut over = self.loaded_posters.len() - self.poster_cap;
            for (_, key) in candidates {
                if over == 0 {
                    break;
                }
                self.loaded_posters.remove(&key);
                to_evict.push(key);
                over -= 1;
            }
        }

        (to_request, to_evict)
    }

    /// Marks a poster upload finished and returns any LRU keys that must be freed.
    pub fn mark_poster_loaded(&mut self, title_id: &str) -> Vec<String> {
        self.tick += 1;
        self.in_flight_posters.remove(title_id);
        self.loaded_posters.insert(String::from(title_id), self.tick);

        let mut evicted = Vec::new();
        if self.loaded_posters.len() > self.poster_cap {
            let mut entries: Vec<(u64, String)> = self
                .loaded_posters
                .iter()
                .filter(|(k, _)| k.as_str() != title_id)
                .map(|(k, &ts)| (ts, k.clone()))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

            let mut over = self.loaded_posters.len() - self.poster_cap;
            for (_, key) in entries {
                if over == 0 {
                    break;
                }
                self.loaded_posters.remove(&key);
                evicted.push(key);
                over -= 1;
            }
        }
        evicted
    }

    pub fn mark_poster_failed(&mut self, title_id: &str) {
        self.in_flight_posters.remove(title_id);
    }

    /// Plans a backdrop request if needed (does not evict existing until loaded or over cap).
    pub fn plan_backdrop(&mut self, desired_id: Option<&str>) -> Option<String> {
        let tid = desired_id?;
        self.tick += 1;
        if let Some(ts) = self.loaded_backdrops.get_mut(tid) {
            *ts = self.tick;
            return None;
        }
        if self.in_flight_backdrop.as_deref() == Some(tid) {
            return None;
        }
        self.in_flight_backdrop = Some(String::from(tid));
        Some(String::from(tid))
    }

    /// Records an uploaded backdrop and returns evicted backdrop Title IDs.
    pub fn mark_backdrop_loaded(&mut self, title_id: &str) -> Vec<String> {
        self.tick += 1;
        if self.in_flight_backdrop.as_deref() == Some(title_id) {
            self.in_flight_backdrop = None;
        }
        self.loaded_backdrops
            .insert(String::from(title_id), self.tick);

        let mut evicted = Vec::new();
        if self.loaded_backdrops.len() > self.backdrop_cap {
            let mut entries: Vec<(u64, String)> = self
                .loaded_backdrops
                .iter()
                .filter(|(k, _)| k.as_str() != title_id)
                .map(|(k, &ts)| (ts, k.clone()))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

            let mut over = self.loaded_backdrops.len() - self.backdrop_cap;
            for (_, key) in entries {
                if over == 0 {
                    break;
                }
                self.loaded_backdrops.remove(&key);
                evicted.push(key);
                over -= 1;
            }
        }
        evicted
    }

    pub fn mark_backdrop_failed(&mut self, title_id: &str) {
        if self.in_flight_backdrop.as_deref() == Some(title_id) {
            self.in_flight_backdrop = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_48_slot_lru_poster_eviction_and_dedup() {
        let mut tracker = TextureCacheTracker::new(48, 1);

        for i in 0..55u32 {
            let tid = format!("{i:08X}");
            let (req, _) = tracker.visible_window_plan(&[tid.clone()], &[]);
            assert_eq!(req, vec![tid.clone()]);
            // Re-planning while in-flight must not duplicate requests.
            let (req_dup, _) = tracker.visible_window_plan(&[tid.clone()], &[]);
            assert!(req_dup.is_empty());

            let _evicted = tracker.mark_poster_loaded(&tid);
            assert!(tracker.loaded_poster_count() <= 48);
        }

        // First 7 IDs (0..7) should have been evicted; 7..55 remain.
        assert!(!tracker.is_poster_loaded("00000000"));
        assert!(!tracker.is_poster_loaded("00000006"));
        assert!(tracker.is_poster_loaded("00000007"));
        assert!(tracker.is_poster_loaded("00000036"));

        // Touching "00000007" keeps it fresh when loading one more poster.
        let (req, _) = tracker.visible_window_plan(&[String::from("00000007")], &[]);
        assert!(req.is_empty());
        let ev = tracker.mark_poster_loaded("00000099");
        assert_eq!(ev, vec![String::from("00000008")]);
        assert!(tracker.is_poster_loaded("00000007"));
    }

    #[test]
    fn enforces_single_slot_backdrop_cache() {
        let mut tracker = TextureCacheTracker::default();
        assert_eq!(
            tracker.plan_backdrop(Some("45410912")),
            Some(String::from("45410912"))
        );
        assert_eq!(tracker.plan_backdrop(Some("45410912")), None);
        assert!(tracker.mark_backdrop_loaded("45410912").is_empty());
        assert!(tracker.is_backdrop_loaded("45410912"));

        assert_eq!(
            tracker.plan_backdrop(Some("584108B7")),
            Some(String::from("584108B7"))
        );
        let evicted = tracker.mark_backdrop_loaded("584108B7");
        assert_eq!(evicted, vec![String::from("45410912")]);
        assert!(!tracker.is_backdrop_loaded("45410912"));
        assert!(tracker.is_backdrop_loaded("584108B7"));
    }
}
