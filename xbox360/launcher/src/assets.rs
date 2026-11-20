//! UI-thread GPU texture cache manager bounded to 48 posters and 1 backdrop.

use std::collections::{HashMap, HashSet};

use launcher_core::cache::{DEFAULT_BACKDROP_CAP, DEFAULT_POSTER_CAP, TextureCacheTracker};
use launcher_core::catalog::{Catalog, GameEntry};
use launcher_core::nav::{GRID_COLS, VISIBLE_ROWS};

/// Premultiplied RGBA8 pixels ready for `egui::ColorImage`.
#[derive(Debug, Clone)]
pub struct DecodedRgba {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

pub struct TextureManager {
    tracker: TextureCacheTracker,
    posters: HashMap<String, egui::TextureHandle>,
    backdrops: HashMap<String, egui::TextureHandle>,
    failed_posters: HashSet<String>,
    failed_backdrops: HashSet<String>,
}

impl Default for TextureManager {
    fn default() -> Self {
        Self {
            tracker: TextureCacheTracker::new(DEFAULT_POSTER_CAP, DEFAULT_BACKDROP_CAP),
            posters: HashMap::new(),
            backdrops: HashMap::new(),
            failed_posters: HashSet::new(),
            failed_backdrops: HashSet::new(),
        }
    }
}

impl TextureManager {
    pub fn poster(&self, title_id: &str) -> Option<&egui::TextureHandle> {
        self.posters.get(title_id)
    }

    pub fn backdrop(&self, title_id: &str) -> Option<&egui::TextureHandle> {
        self.backdrops.get(title_id)
    }

    pub fn reset_failures(&mut self) {
        self.failed_posters.clear();
        self.failed_backdrops.clear();
    }

    pub fn clear_all(&mut self) {
        self.posters.clear();
        self.backdrops.clear();
        self.failed_posters.clear();
        self.failed_backdrops.clear();
        self.tracker = TextureCacheTracker::new(DEFAULT_POSTER_CAP, DEFAULT_BACKDROP_CAP);
    }

    pub fn ingest_poster(
        &mut self,
        ctx: &egui::Context,
        title_id: &str,
        decoded: Option<DecodedRgba>,
    ) {
        let Some(img) = decoded else {
            self.tracker.mark_poster_failed(title_id);
            self.failed_posters.insert(String::from(title_id));
            return;
        };
        let color = egui::ColorImage::from_rgba_premultiplied([img.width, img.height], &img.pixels);
        let handle = ctx.load_texture(
            format!("poster_{title_id}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.posters.insert(String::from(title_id), handle);
        for ev in self.tracker.mark_poster_loaded(title_id) {
            self.posters.remove(&ev);
        }
    }

    pub fn ingest_backdrop(
        &mut self,
        ctx: &egui::Context,
        title_id: &str,
        decoded: Option<DecodedRgba>,
    ) {
        let Some(img) = decoded else {
            self.tracker.mark_backdrop_failed(title_id);
            self.failed_backdrops.insert(String::from(title_id));
            return;
        };
        let color = egui::ColorImage::from_rgba_premultiplied([img.width, img.height], &img.pixels);
        let handle = ctx.load_texture(
            format!("backdrop_{title_id}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.backdrops.insert(String::from(title_id), handle);
        for ev in self.tracker.mark_backdrop_loaded(title_id) {
            self.backdrops.remove(&ev);
        }
    }

    /// Computes visible + prefetch artwork loads to enqueue on the worker thread.
    pub fn plan_loads(
        &mut self,
        filtered: &[usize],
        catalog: &Catalog,
        scroll_row: usize,
        selected_entry: Option<&GameEntry>,
        online_catalog: bool,
    ) -> (Vec<(String, bool)>, Option<(String, bool)>) {
        let start_idx = scroll_row.saturating_mul(GRID_COLS);
        let visible_end = scroll_row
            .saturating_add(VISIBLE_ROWS)
            .saturating_mul(GRID_COLS)
            .min(filtered.len());

        let mut remote_flags: HashMap<String, bool> = HashMap::new();
        let mut visible_ids = Vec::new();
        if start_idx < visible_end {
            for &entry_idx in &filtered[start_idx..visible_end] {
                if let Some(entry) = catalog.get(entry_idx)
                    && !self.failed_posters.contains(&entry.title_id)
                {
                    visible_ids.push(entry.title_id.clone());
                    remote_flags.insert(entry.title_id.clone(), online_catalog && entry.has_poster);
                }
            }
        }

        let mut prefetch_ids = Vec::new();
        let prev_start = scroll_row.saturating_sub(1).saturating_mul(GRID_COLS);
        let prev_end = start_idx.min(filtered.len());
        if scroll_row > 0 && prev_start < prev_end {
            for &entry_idx in &filtered[prev_start..prev_end] {
                if let Some(entry) = catalog.get(entry_idx)
                    && !self.failed_posters.contains(&entry.title_id)
                {
                    prefetch_ids.push(entry.title_id.clone());
                    remote_flags
                        .entry(entry.title_id.clone())
                        .or_insert(online_catalog && entry.has_poster);
                }
            }
        }
        let next_end = (visible_end + GRID_COLS).min(filtered.len());
        if visible_end < next_end {
            for &entry_idx in &filtered[visible_end..next_end] {
                if let Some(entry) = catalog.get(entry_idx)
                    && !self.failed_posters.contains(&entry.title_id)
                {
                    prefetch_ids.push(entry.title_id.clone());
                    remote_flags
                        .entry(entry.title_id.clone())
                        .or_insert(online_catalog && entry.has_poster);
                }
            }
        }

        let (to_req, to_evict) = self.tracker.visible_window_plan(&visible_ids, &prefetch_ids);
        for ev in to_evict {
            self.posters.remove(&ev);
        }
        let poster_jobs = to_req
            .into_iter()
            .map(|tid| {
                let remote = remote_flags.get(&tid).copied().unwrap_or(false);
                (tid, remote)
            })
            .collect();

        let desired_backdrop = selected_entry.and_then(|entry| {
            if self.failed_backdrops.contains(&entry.title_id) {
                None
            } else {
                Some(entry.title_id.as_str())
            }
        });
        let backdrop_job = self.tracker.plan_backdrop(desired_backdrop).map(|tid| {
            let remote = selected_entry
                .map(|e| online_catalog && e.has_backdrop)
                .unwrap_or(false);
            (tid, remote)
        });

        (poster_jobs, backdrop_job)
    }
}
