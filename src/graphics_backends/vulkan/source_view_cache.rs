//! Bounded cache of views of game-owned source images.
//!
//! Game textures can be recreated while the output swapchain stays alive
//! (dynamic resolution). A driver may reuse the same `VkImage` handle for the
//! new image, so the extent is part of the cache key. Views are only destroyed
//! by `clear`/`drain` at frame boundaries, never while recording an eye.

use ash::vk::{self, Handle};
use std::collections::HashMap;

pub(super) type SourceExtent = (u32, u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct SourceKey {
    image: u64,
    format: i32,
    layer: u32,
    extent: SourceExtent,
}

impl SourceKey {
    pub(super) fn new(
        image: vk::Image,
        format: vk::Format,
        layer: u32,
        extent: SourceExtent,
    ) -> Self {
        Self {
            image: image.as_raw(),
            format: format.as_raw(),
            layer,
            extent,
        }
    }
}

#[derive(Default)]
pub(super) struct SourceViewCache {
    views: HashMap<SourceKey, vk::ImageView>,
}

impl SourceViewCache {
    pub(super) fn get(&self, key: SourceKey) -> Option<vk::ImageView> {
        self.views.get(&key).copied()
    }

    pub(super) fn insert(&mut self, key: SourceKey, view: vk::ImageView) {
        let previous = self.views.insert(key, view);
        debug_assert!(previous.is_none(), "source view inserted twice");
    }

    pub(super) fn count(&self) -> usize {
        self.views.len()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = vk::ImageView> + '_ {
        self.views.values().copied()
    }

    pub(super) fn drain(&mut self) -> impl Iterator<Item = vk::ImageView> + '_ {
        self.views.drain().map(|(_, view)| view)
    }
}

/// Tracks whether an observed extent differs from the previous one. The dirty
/// flag is consumed at a safe point so the caller can invalidate once.
#[derive(Default)]
pub(super) struct ExtentTracker {
    extent: Option<SourceExtent>,
    dirty: bool,
}

impl ExtentTracker {
    pub(super) fn observe(&mut self, extent: SourceExtent) {
        if self.extent != Some(extent) {
            self.extent = Some(extent);
            self.dirty = true;
        }
    }

    pub(super) fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(raw: u64) -> vk::ImageView {
        vk::ImageView::from_raw(raw)
    }

    fn key(extent: SourceExtent) -> SourceKey {
        SourceKey::new(
            vk::Image::from_raw(7),
            vk::Format::R8G8B8A8_UNORM,
            0,
            extent,
        )
    }

    #[test]
    fn cache_hit_requires_matching_extent() {
        let mut cache = SourceViewCache::default();
        cache.insert(key((100, 100)), view(1));

        assert_eq!(
            cache.get(key((100, 100))).map(|view| view.as_raw()),
            Some(1)
        );
        assert!(cache.get(key((200, 200))).is_none());
    }

    #[test]
    fn cache_keys_distinguish_layers_and_formats() {
        let mut cache = SourceViewCache::default();
        let layer_one = SourceKey::new(
            vk::Image::from_raw(7),
            vk::Format::R8G8B8A8_UNORM,
            1,
            (100, 100),
        );
        let other_format = SourceKey::new(
            vk::Image::from_raw(7),
            vk::Format::B8G8R8A8_UNORM,
            0,
            (100, 100),
        );
        cache.insert(key((100, 100)), view(1));

        assert!(cache.get(layer_one).is_none());
        assert!(cache.get(other_format).is_none());
    }

    #[test]
    fn extent_tracker_reports_changes_once() {
        let mut tracker = ExtentTracker::default();
        assert!(!tracker.take_dirty());

        tracker.observe((100, 100));
        assert!(tracker.take_dirty());
        tracker.observe((100, 100));
        assert!(!tracker.take_dirty());

        tracker.observe((200, 200));
        assert!(tracker.take_dirty());
        // A -> B -> A is another change, even though A was seen before.
        tracker.observe((100, 100));
        assert!(tracker.take_dirty());
    }

    #[test]
    fn drain_returns_all_views_and_empties_the_cache() {
        let mut cache = SourceViewCache::default();
        cache.insert(key((100, 100)), view(1));
        cache.insert(key((200, 200)), view(2));

        assert_eq!(cache.iter().count(), 2);
        let raw: Vec<_> = cache.drain().map(|view| view.as_raw()).collect();
        assert_eq!(raw.len(), 2);
        assert!(raw.contains(&1));
        assert!(raw.contains(&2));
        assert_eq!(cache.count(), 0);
    }
}
