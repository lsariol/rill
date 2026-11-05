//! A bounded image cache for emotes and badges: least recently used images are unloaded.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    App, AppContext, Entity, ImageCache, ImageCacheError, ImageCacheItem, RenderImage, Resource,
    Window,
};

/// Keeps at most `limit` images, unloading the ones used longest ago.
pub struct BoundedImageCache {
    items: HashMap<Resource, (ImageCacheItem, u64)>,
    limit: usize,
    clock: u64,
}

impl BoundedImageCache {
    pub fn new(limit: usize, cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| BoundedImageCache {
            items: HashMap::new(),
            limit: limit.max(16),
            clock: 0,
        });
        cx.observe_release(&cache, |cache, cx| {
            for (_, (item, _)) in cache.items.drain() {
                if let Some(Ok(image)) = item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        cache
    }

    /// The number of images held.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    fn evict(&mut self, window: &mut Window, cx: &mut App) {
        while self.items.len() > self.limit {
            let Some(oldest) = self
                .items
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(r, _)| r.clone())
            else {
                return;
            };
            if let Some((item, _)) = self.items.remove(&oldest)
                && let Some(Ok(image)) = item.get()
            {
                cx.drop_image(image, Some(window));
            }
        }
    }
}

impl ImageCache for BoundedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.clock += 1;
        let clock = self.clock;
        if !self.items.contains_key(resource) {
            self.items
                .insert(resource.clone(), (ImageCacheItem::new(resource, cx), clock));
            self.evict(window, cx);
        }
        let (item, used) = self.items.get_mut(resource)?;
        *used = clock;
        item.use_image(window)
    }
}
