//! A bounded image cache for emotes and badges: least recently used images are unloaded.
//!
//! Images drawn in chat are shrunk to the largest size chat draws them at, so a big animated
//! emote doesn't keep every frame at full size. Rill decodes downloaded images itself and stops
//! at an animation's first broken frame: GPUI skips broken frames, and some broken GIFs report
//! the same error forever, which kept a core busy until Rill closed.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;

use futures::AsyncReadExt as _;
use gpui::http_client::{AsyncBody, HttpClient};

use gpui::{
    App, AppContext, Asset, Entity, EntityId, ImageAssetLoader, ImageCache, ImageCacheError,
    RenderImage, Resource, SharedUri, Task, Window,
};
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, DynamicImage, Frame, ImageBuffer, ImageFormat, Rgba, imageops};

/// Marks an image address as drawn in chat. The cache reads it; it's never fetched.
const CHAT: &str = "#chat-size";

/// The tallest chat draws an image, before display scaling: an emote at the largest text size.
const CHAT_MAX_PX: f32 = 40.;

/// The address of an image drawn in chat, which the cache keeps at chat size.
pub fn chat_sized(url: &str) -> String {
    format!("{url}{CHAT}")
}

type Loaded = Result<Arc<RenderImage>, ImageCacheError>;

/// Keeps at most `limit` images, unloading the ones used longest ago.
pub struct BoundedImageCache {
    items: HashMap<Resource, (Load, u64)>,
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
            let max_px = (CHAT_MAX_PX * window.scale_factor()).ceil() as u32;
            self.items
                .insert(resource.clone(), (Load::start(resource, max_px, cx), clock));
            self.evict(window, cx);
        }
        let (item, used) = self.items.get_mut(resource)?;
        *used = clock;
        item.use_by(window.current_view())
    }
}

/// What to fetch for a cache key, and whether to shrink it to chat size.
fn source(resource: &Resource) -> (Resource, bool) {
    if let Resource::Uri(uri) = resource {
        let uri = uri.to_string();
        if let Some(clean) = uri.strip_suffix(CHAT) {
            return (Resource::Uri(SharedUri::from(clean.to_string())), true);
        }
    }
    (resource.clone(), false)
}

enum State {
    /// Still loading: the views to redraw once it's done.
    Loading(HashSet<EntityId>),
    Done(Loaded),
}

/// One image being loaded, decoded and, for chat, shrunk, off the main thread.
struct Load {
    state: Rc<RefCell<State>>,
    _task: Task<()>,
}

impl Load {
    fn start(resource: &Resource, max_px: u32, cx: &mut App) -> Load {
        let (fetch, shrink) = source(resource);
        // GPUI's own loader, for embedded images and anything that isn't a bitmap (SVG);
        // it only runs if awaited.
        let gpui_load = ImageAssetLoader::load(fetch.clone(), cx);
        let client = cx.http_client();
        let work = cx.background_executor().spawn(async move {
            let image = match &fetch {
                Resource::Uri(uri) => {
                    let bytes = download(client.as_ref(), &uri.to_string()).await?;
                    match image::guess_format(&bytes) {
                        Ok(format) => decode(&bytes, format)?,
                        Err(_) => gpui_load.await?,
                    }
                }
                _ => gpui_load.await?,
            };
            Ok(if shrink { shrunk(image, max_px) } else { image })
        });
        let state = Rc::new(RefCell::new(State::Loading(HashSet::new())));
        let weak = Rc::downgrade(&state);
        let task = cx.spawn(async move |cx| {
            let result = work.await;
            let Some(state) = weak.upgrade() else {
                return;
            };
            let previous = std::mem::replace(&mut *state.borrow_mut(), State::Done(result));
            if let State::Loading(views) = previous {
                cx.update(|cx| {
                    for view in views {
                        cx.notify(view);
                    }
                });
            }
        });
        Load { state, _task: task }
    }

    fn get(&self) -> Option<Loaded> {
        match &*self.state.borrow() {
            State::Loading(_) => None,
            State::Done(result) => Some(result.clone()),
        }
    }

    /// The image if it's ready; otherwise `view` is redrawn when it is.
    fn use_by(&self, view: EntityId) -> Option<Loaded> {
        match &mut *self.state.borrow_mut() {
            State::Loading(views) => {
                views.insert(view);
                None
            }
            State::Done(result) => Some(result.clone()),
        }
    }
}

/// An image's bytes from the web.
async fn download(client: &dyn HttpClient, uri: &str) -> Result<Vec<u8>, ImageCacheError> {
    let mut response = client
        .get(uri, AsyncBody::from(()), true)
        .await
        .map_err(|e| failed(format!("loading {uri}: {e}")))?;
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .map_err(|e| failed(format!("reading {uri}: {e}")))?;
    if !response.status().is_success() {
        return Err(failed(format!("{uri} answered {}", response.status())));
    }
    Ok(body)
}

fn failed(why: String) -> ImageCacheError {
    ImageCacheError::Io(Arc::new(std::io::Error::other(why)))
}

/// Decodes a bitmap into frames GPUI can draw (BGRA), every frame of an animation.
fn decode(bytes: &[u8], format: ImageFormat) -> Result<Arc<RenderImage>, ImageCacheError> {
    let bad = |e: image::ImageError| failed(format!("decoding a {format:?} image: {e}"));
    let mut frames = match format {
        ImageFormat::Gif => frames_until_broken(
            GifDecoder::new(Cursor::new(bytes))
                .map_err(bad)?
                .into_frames(),
        ),
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(bad)?;
            if decoder.has_animation() {
                let _ = decoder.set_background_color(Rgba([0, 0, 0, 0]));
                frames_until_broken(decoder.into_frames())
            } else {
                vec![Frame::new(
                    DynamicImage::from_decoder(decoder).map_err(bad)?.to_rgba8(),
                )]
            }
        }
        _ => vec![Frame::new(
            image::load_from_memory_with_format(bytes, format)
                .map_err(bad)?
                .to_rgba8(),
        )],
    };
    if frames.is_empty() {
        return Err(failed(format!(
            "no frame of the {format:?} image could be decoded"
        )));
    }
    for frame in &mut frames {
        for pixel in frame.buffer_mut().pixels_mut() {
            pixel.0.swap(0, 2);
        }
    }
    Ok(Arc::new(RenderImage::new(frames)))
}

/// Errors in a row after which an animation is cut short.
const BROKEN_FRAMES: usize = 3;

/// An animation's frames, skipping a broken one but stopping once several fail in a row:
/// a cut-off file reports the same error forever.
fn frames_until_broken(frames: image::Frames<'_>) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut failures = 0;
    for frame in frames {
        match frame {
            Ok(frame) => {
                failures = 0;
                out.push(frame);
            }
            Err(e) => {
                failures += 1;
                if failures == BROKEN_FRAMES {
                    tracing::debug!(err = %e, frames = out.len(), "animated image cut short");
                    break;
                }
            }
        }
    }
    out
}

/// The image at most `max_px` tall, every frame scaled alike; smaller images as they are.
fn shrunk(image: Arc<RenderImage>, max_px: u32) -> Arc<RenderImage> {
    let count = image.frame_count();
    let h = image.size(0).height.0.max(1) as u32;
    if count == 0 || h <= max_px {
        return image;
    }
    let scale = max_px as f32 / h as f32;
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let size = image.size(i);
        let (fw, fh) = (size.width.0.max(1) as u32, size.height.0.max(1) as u32);
        let Some(bytes) = image.as_bytes(i) else {
            return image;
        };
        let Some(mut full) = ImageBuffer::<Rgba<u8>, _>::from_raw(fw, fh, bytes.to_vec()) else {
            return image;
        };
        // Blend with alpha taken into account, so transparent pixels don't darken the edges.
        premultiply(&mut full);
        let tw = ((fw as f32 * scale).round() as u32).max(1);
        let th = ((fh as f32 * scale).round() as u32).max(1);
        let mut small = imageops::resize(&full, tw, th, imageops::FilterType::Triangle);
        unpremultiply(&mut small);
        frames.push(Frame::from_parts(small, 0, 0, image.delay(i)));
    }
    Arc::new(RenderImage::new(frames))
}

fn premultiply(image: &mut ImageBuffer<Rgba<u8>, Vec<u8>>) {
    for p in image.pixels_mut() {
        let a = u16::from(p[3]);
        for c in 0..3 {
            p[c] = ((u16::from(p[c]) * a + 127) / 255) as u8;
        }
    }
}

fn unpremultiply(image: &mut ImageBuffer<Rgba<u8>, Vec<u8>>) {
    for p in image.pixels_mut() {
        let a = u16::from(p[3]);
        if a == 0 {
            continue;
        }
        for c in 0..3 {
            p[c] = ((u16::from(p[c]) * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Delay;

    fn image(w: u32, h: u32, frames: usize) -> Arc<RenderImage> {
        let frames: Vec<Frame> = (0..frames)
            .map(|i| {
                let buf = ImageBuffer::from_pixel(w, h, Rgba([10 * i as u8, 20, 30, 255]));
                Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(40, 1))
            })
            .collect();
        Arc::new(RenderImage::new(frames))
    }

    fn gif(frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut out);
            for i in 0..frames {
                let buf = ImageBuffer::from_pixel(8, 8, Rgba([40 * i as u8, 90, 160, 255]));
                encoder.encode_frame(Frame::new(buf)).unwrap();
            }
        }
        out
    }

    #[test]
    fn a_cut_off_gif_stops_instead_of_decoding_forever() {
        let mut bytes = gif(3);
        bytes.truncate(bytes.len() - 2);
        // The decoder itself reports the same error again and again.
        let decoder = GifDecoder::new(Cursor::new(&bytes)).unwrap();
        let errors = decoder
            .into_frames()
            .take(50)
            .filter(Result::is_err)
            .count();
        assert!(errors > 40, "{errors}");
        let image = decode(&bytes, ImageFormat::Gif).unwrap();
        assert_eq!(image.frame_count(), 2);
    }

    #[test]
    fn skips_a_broken_frame_but_gives_up_on_a_run_of_them() {
        let ok = || {
            Ok(Frame::new(ImageBuffer::from_pixel(
                1,
                1,
                Rgba([0, 0, 0, 255]),
            )))
        };
        let bad = || {
            Err(image::ImageError::IoError(std::io::Error::other(
                "broken frame",
            )))
        };
        let some = vec![ok(), bad(), ok(), bad(), bad(), ok()];
        assert_eq!(
            frames_until_broken(image::Frames::new(Box::new(some.into_iter()))).len(),
            3
        );
        let forever = std::iter::once(ok()).chain(std::iter::repeat_with(bad));
        assert_eq!(
            frames_until_broken(image::Frames::new(Box::new(forever))).len(),
            1
        );
    }

    #[test]
    fn decodes_every_frame_of_a_gif_in_gpuis_byte_order() {
        let image = decode(&gif(3), ImageFormat::Gif).unwrap();
        assert_eq!(image.frame_count(), 3);
        let px = &image.as_bytes(0).unwrap()[..4];
        // Red and blue swapped: 0 red, 160 blue becomes [160, _, 0, _].
        assert!(px[0] > 120 && px[2] < 40, "{px:?}");
    }

    #[test]
    fn shrinks_every_frame_of_a_big_emote_to_chat_size() {
        let small = shrunk(image(250, 250, 3), 40);
        assert_eq!(small.frame_count(), 3);
        for i in 0..3 {
            let s = small.size(i);
            assert_eq!((s.width.0, s.height.0), (40, 40));
            assert_eq!(small.delay(i), Delay::from_numer_denom_ms(40, 1));
        }
        assert_eq!(&small.as_bytes(2).unwrap()[..4], &[20, 20, 30, 255]);
    }

    #[test]
    fn keeps_wide_emotes_in_proportion_and_small_ones_as_they_are() {
        let wide = shrunk(image(300, 100, 1), 40);
        assert_eq!((wide.size(0).width.0, wide.size(0).height.0), (120, 40));
        let already = image(28, 28, 1);
        assert!(Arc::ptr_eq(&shrunk(already.clone(), 40), &already));
    }

    #[test]
    fn edges_keep_their_color_where_transparency_meets_color() {
        let mut buf = ImageBuffer::from_pixel(80, 80, Rgba([200, 100, 50, 0]));
        for x in 0..40 {
            for y in 0..80 {
                buf.put_pixel(x, y, Rgba([200, 100, 50, 255]));
            }
        }
        let img = Arc::new(RenderImage::new(vec![Frame::new(buf)]));
        let small = shrunk(img, 40);
        let bytes = small.as_bytes(0).unwrap();
        // The pixel straddling the edge is half see-through but still the same color.
        let edge = &bytes[(20 * 40 + 19) * 4..(20 * 40 + 20) * 4];
        assert!(edge[3] > 0 && edge[3] < 255, "{edge:?}");
        assert!((i32::from(edge[0]) - 200).abs() <= 2, "{edge:?}");
    }

    #[test]
    fn only_chat_addresses_are_shrunk_and_the_mark_is_never_fetched() {
        let chat = Resource::Uri(SharedUri::from(chat_sized(
            "https://files.kick.com/emotes/1/fullsize",
        )));
        let (fetch, shrink) = source(&chat);
        assert!(shrink);
        assert_eq!(
            fetch,
            Resource::Uri(SharedUri::from("https://files.kick.com/emotes/1/fullsize"))
        );
        let card = Resource::Uri(SharedUri::from("https://files.kick.com/emotes/1/fullsize"));
        assert_eq!(source(&card), (card.clone(), false));
    }
}
