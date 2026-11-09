//! A chat line's text as one selectable run. Names and links are clickable, and emote images are
//! drawn over space kept for them in the text, so selecting and copying run straight through them
//! (an emote copies as its name).

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, BorderStyle, Bounds, Corners, CursorStyle, Edges, Element,
    ElementId, GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, SharedString, StyledText, TextLayout, TextStyleRefinement, Window, point, px,
    transparent_black,
};
use gpui_base::{TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun};

use crate::{row, style};

/// What a click on part of the text does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    /// The author's name: opens the user card.
    Name,
    /// Opens the address in the browser.
    Link(String),
    /// An emote image, by its index among the line's images: hovering shows it bigger.
    Emote(usize),
}

/// What the pointer is over, for the card that floats beside it. `at` is where the thing
/// starts, in window coordinates.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hover {
    Link {
        url: String,
        at: Point<Pixels>,
    },
    /// An emote, with any zero-width emotes drawn over it.
    Emote {
        names: Vec<String>,
        url: String,
        size: Option<(u16, u16)>,
        scope: rill_core::EmoteScope,
        at: Point<Pixels>,
    },
    Badge {
        title: String,
        url: Option<String>,
        at: Point<Pixels>,
    },
}

/// Called with what was clicked and where, in window coordinates.
pub(crate) type ClickFn = Rc<dyn Fn(&Target, Point<Pixels>, &mut Window, &mut App)>;
/// Called when the pointer moves onto something with a hover card, or off it (`None`). The
/// first argument names where it came from, so leaving one thing can't hide the next one's card.
pub(crate) type HoverFn = Rc<dyn Fn(SharedString, Option<Hover>, &mut Window, &mut App)>;

/// A piece of the line.
pub(crate) enum Part {
    Text {
        text: String,
        style: Option<HighlightStyle>,
        target: Option<Target>,
    },
    Emote {
        name: String,
        url: String,
        size: Option<(u16, u16)>,
        /// Zero-width: drawn over the emote before it.
        overlay: bool,
        scope: rill_core::EmoteScope,
    },
}

/// The pieces of a line, built up in order.
#[derive(Default)]
pub(crate) struct Parts(Vec<Part>);

impl Parts {
    pub fn text(&mut self, text: impl Into<String>, style: Option<HighlightStyle>) {
        self.0.push(Part::Text {
            text: text.into(),
            style,
            target: None,
        });
    }

    pub fn target(&mut self, text: impl Into<String>, style: HighlightStyle, target: Target) {
        self.0.push(Part::Text {
            text: text.into(),
            style: Some(style),
            target: Some(target),
        });
    }

    /// Text with any web addresses in it made into links.
    pub fn linked(&mut self, text: &str, style: Option<HighlightStyle>) {
        let mut plain = String::new();
        for (i, word) in text.split(' ').enumerate() {
            if i > 0 {
                plain.push(' ');
            }
            match link_in(word) {
                Some((url, used)) => {
                    if !plain.is_empty() {
                        self.text(std::mem::take(&mut plain), style);
                    }
                    self.link(&word[..used], url);
                    plain.push_str(&word[used..]);
                }
                None => plain.push_str(word),
            }
        }
        if !plain.is_empty() {
            self.text(plain, style);
        }
    }

    /// A link showing `text` that opens `url`.
    pub fn link(&mut self, text: &str, url: String) {
        self.target(text, link_style(), Target::Link(url));
    }

    pub fn emote(
        &mut self,
        name: &str,
        url: &str,
        size: Option<(u16, u16)>,
        overlay: bool,
        scope: rill_core::EmoteScope,
    ) {
        self.0.push(Part::Emote {
            name: name.into(),
            url: url.into(),
            size,
            overlay,
            scope,
        });
    }
}

/// The address a word links to, and how much of the word is the link (trailing punctuation
/// isn't). Only web addresses: `http://`, `https://` or `www.`.
pub(crate) fn link_in(word: &str) -> Option<(String, usize)> {
    let lower = word.to_ascii_lowercase();
    let web = lower.starts_with("https://") || lower.starts_with("http://");
    if !web && !lower.starts_with("www.") {
        return None;
    }
    let mut end = word.len();
    while let Some(c) = word[..end].chars().last() {
        let unbalanced =
            c == ')' && word[..end].matches('(').count() < word[..end].matches(')').count();
        if matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | '\'' | '"') || unbalanced {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    let shown = &word[..end];
    let host = shown.split("://").nth(1).unwrap_or(shown);
    if host.len() < 4 || !host.contains('.') {
        return None;
    }
    let url = if web {
        shown.to_string()
    } else {
        format!("https://{shown}")
    };
    Some((url, end))
}

fn link_style() -> HighlightStyle {
    HighlightStyle {
        color: Some(style::hsla(style::LINK)),
        underline: Some(gpui::UnderlineStyle {
            thickness: px(1.),
            ..Default::default()
        }),
        ..HighlightStyle::default()
    }
}

/// The line as laid out: text with placeholders where images go.
#[derive(Default)]
struct Built {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    targets: Vec<(Range<usize>, Target)>,
    /// Placeholder ranges and the emote name each copies as.
    copies: Vec<(Range<usize>, String)>,
    images: Vec<Image>,
}

struct Image {
    /// The placeholder it's drawn over.
    at: Range<usize>,
    /// The placeholder's width.
    slot: Pixels,
    name: String,
    url: String,
    size: Option<(u16, u16)>,
    scope: rill_core::EmoteScope,
}

/// Placeholder characters: digits are as wide as each other and never split by wrapping.
const SLOT_CHAR: char = '0';

fn build(parts: &[Part], digit: Pixels) -> Built {
    let mut b = Built::default();
    let mut base: Option<(Range<usize>, Pixels)> = None;
    // Highlight colors blend over the text's, so only fading hides the placeholder.
    let hidden = HighlightStyle {
        fade_out: Some(1.),
        ..HighlightStyle::default()
    };
    for part in parts {
        match part {
            Part::Text {
                text,
                style,
                target,
            } => {
                if !text.trim().is_empty() {
                    base = None;
                }
                let start = b.text.len();
                b.text.push_str(text);
                let range = start..b.text.len();
                if let Some(s) = style {
                    b.highlights.push((range.clone(), *s));
                }
                if let Some(t) = target {
                    b.targets.push((range, t.clone()));
                }
            }
            Part::Emote {
                name,
                url,
                size,
                overlay,
                scope,
            } => {
                if *overlay && let Some((at, slot)) = base.clone() {
                    b.images.push(Image {
                        at,
                        slot,
                        name: name.clone(),
                        url: url.clone(),
                        size: *size,
                        scope: *scope,
                    });
                    continue;
                }
                let width = row::emote_size(*size).width;
                let n = (f32::from(width) / f32::from(digit)).ceil().max(1.) as usize;
                let start = b.text.len();
                b.text.extend(std::iter::repeat_n(SLOT_CHAR, n));
                let at = start..b.text.len();
                b.highlights.push((at.clone(), hidden));
                b.copies.push((at.clone(), name.clone()));
                let slot = digit * n as f32;
                b.targets.push((at.clone(), Target::Emote(b.images.len())));
                b.images.push(Image {
                    at: at.clone(),
                    slot,
                    name: name.clone(),
                    url: url.clone(),
                    size: *size,
                    scope: *scope,
                });
                base = Some((at, slot));
            }
        }
    }
    b
}

/// The text a selection copies: placeholders become emote names.
fn copy_text(text: &str, copies: &[(Range<usize>, String)], range: Range<usize>) -> String {
    let mut out = String::new();
    let mut pos = range.start;
    for (at, name) in copies {
        if at.end <= range.start || at.start >= range.end {
            continue;
        }
        if at.start > pos {
            out.push_str(&text[pos..at.start]);
        }
        out.push_str(name);
        pos = pos.max(at.end);
    }
    if pos < range.end {
        out.push_str(&text[pos..range.end]);
    }
    out
}

/// An emote's hover card: its name and any zero-width emotes on it, its image, size and scope.
type EmoteCard = (
    Vec<String>,
    String,
    Option<(u16, u16)>,
    rill_core::EmoteScope,
);

/// Placeholder ranges and the emote name each copies as.
type Copies = Vec<(Range<usize>, String)>;

/// What a line copies as.
#[derive(Default)]
struct Content {
    text: String,
    copies: Copies,
    /// Put before the line when it's copied from its start: the row's time.
    prefix: Option<String>,
    /// Copies nothing: the time column, which the line next to it copies instead.
    silent: bool,
}

/// What the element keeps between frames.
#[derive(Clone)]
pub(crate) struct State {
    handle: TextSelectionHandle,
    /// The text and placeholders, for copying.
    content: Rc<RefCell<Content>>,
    /// The selected part of this line, as last painted.
    selected: Rc<RefCell<Option<Range<usize>>>>,
    /// Where the left button went down, to tell a click from a drag.
    down: Rc<Cell<Option<Point<Pixels>>>>,
    /// The target under the pointer, by index into the targets.
    hovered: Rc<Cell<Option<usize>>>,
}

/// One chat line's text.
pub(crate) struct ChatText {
    id: ElementId,
    order: u64,
    parts: Parts,
    on_click: Option<ClickFn>,
    on_hover: Option<HoverFn>,
    built: Built,
    styled: StyledText,
    prefix: Option<String>,
    silent: bool,
    /// Names this line to the hover callback.
    key: SharedString,
}

impl ChatText {
    /// A line; `order` is its place among the lines, for copying several in order.
    pub fn new(id: impl Into<ElementId>, order: u64, parts: Parts) -> Self {
        let id = id.into();
        ChatText {
            key: format!("{id:?}").into(),
            id,
            order,
            parts,
            on_click: None,
            on_hover: None,
            built: Built::default(),
            styled: StyledText::new(""),
            prefix: None,
            silent: false,
        }
    }

    /// Copied before the line when the selection includes its start (the row's time).
    pub fn copy_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Selectable, but copies nothing itself.
    pub fn silent(mut self) -> Self {
        self.silent = true;
        self
    }

    pub fn on_click(mut self, f: ClickFn) -> Self {
        self.on_click = Some(f);
        self
    }

    pub fn on_hover(mut self, f: HoverFn) -> Self {
        self.on_hover = Some(f);
        self
    }

    fn target_at(&self, layout: &TextLayout, position: Point<Pixels>) -> Option<usize> {
        let ix = layout.index_for_position(position).ok()?;
        self.built
            .targets
            .iter()
            .position(|(r, _)| r.start <= ix && ix < r.end)
    }

    fn paint_selection(layout: &TextLayout, range: Range<usize>, window: &mut Window) {
        let (Some(start), Some(end)) = (
            layout.position_for_index(range.start),
            layout.position_for_index(range.end),
        ) else {
            return;
        };
        let bounds = layout.bounds();
        let line = layout.line_height();
        let mut quads = Vec::new();
        if start.y == end.y {
            quads.push(Bounds::from_corners(start, point(end.x, end.y + line)));
        } else {
            quads.push(Bounds::from_corners(
                start,
                point(bounds.right(), start.y + line),
            ));
            if end.y > start.y + line {
                quads.push(Bounds::from_corners(
                    point(bounds.left(), start.y + line),
                    point(bounds.right(), end.y),
                ));
            }
            quads.push(Bounds::from_corners(
                point(bounds.left(), end.y),
                point(end.x, end.y + line),
            ));
        }
        for bounds in quads {
            window.paint_quad(PaintQuad {
                bounds,
                background: style::hsla_alpha(style::SELECTION, 0.45).into(),
                corner_radii: Corners::default(),
                border_widths: Edges::default(),
                border_color: transparent_black(),
                border_style: BorderStyle::default(),
            });
        }
    }
}

impl IntoElement for ChatText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ChatText {
    type RequestLayoutState = State;
    type PrepaintState = (Hitbox, Vec<AnyElement>);

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, State) {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let font_id = window.text_system().resolve_font(&text_style.font());
        let digit = window
            .text_system()
            .advance(font_id, font_size, SLOT_CHAR)
            .map(|s| s.width)
            .unwrap_or(font_size * 0.6)
            .max(px(1.));
        self.built = build(&self.parts.0, digit);
        let b = &self.built;
        self.styled = StyledText::new(SharedString::from(b.text.clone()))
            .with_highlights(b.highlights.clone());

        let state = window.with_element_state(
            global_id.expect("ChatText has an id"),
            |kept: Option<State>, _| {
                let state = kept.unwrap_or_else(|| {
                    let handle = TextSelectionHandle::new(b.text.clone(), cx);
                    let state = State {
                        handle,
                        content: Rc::default(),
                        selected: Rc::default(),
                        down: Rc::default(),
                        hovered: Rc::default(),
                    };
                    let (content, selected) = (state.content.clone(), state.selected.clone());
                    state.handle.copy_with(
                        move |_| {
                            let c = &*content.borrow();
                            if c.silent {
                                return String::new();
                            }
                            let range = selected.borrow().clone().unwrap_or(0..c.text.len());
                            let line = copy_text(&c.text, &c.copies, range.clone());
                            match &c.prefix {
                                Some(prefix) if range.start == 0 => format!("{prefix}{line}"),
                                _ => line,
                            }
                        },
                        cx,
                    );
                    state
                });
                (state.clone(), state)
            },
        );
        *state.content.borrow_mut() = Content {
            text: b.text.clone(),
            copies: b.copies.clone(),
            prefix: self.prefix.clone(),
            silent: self.silent,
        };

        // Lines with images are as tall as the images.
        let tall = (!b.images.is_empty()).then(|| TextStyleRefinement {
            line_height: Some(px(row::emote_px() + 2. * row::scale()).into()),
            ..TextStyleRefinement::default()
        });
        let (layout_id, ()) = window.with_text_style(tall, |window| {
            self.styled
                .request_layout(global_id, inspector_id, window, cx)
        });
        (layout_id, state)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut State,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
            .with_document_order(self.order)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(&state.handle, window, cx);
        state.handle.register(registration, window, cx);

        let layout = self.styled.layout().clone();
        let line = layout.line_height();
        let mut images = Vec::new();
        for image in &self.built.images {
            // The slot's second character, so a slot starting a wrapped line isn't placed at the
            // end of the line before.
            let Some(next) = layout.position_for_index(image.at.start + 1) else {
                continue;
            };
            let slot_width = image.slot / (image.at.len() as f32);
            let left = next.x - slot_width;
            let size = row::emote_size(image.size);
            let origin = point(
                left + (image.slot - size.width) / 2.,
                next.y + (line - size.height) / 2.,
            );
            let mut element = row::emote_image(&image.name, Some(&image.url), image.size);
            element.prepaint_as_root(origin, size.map(AvailableSpace::Definite), window, cx);
            images.push(element);
        }
        (hitbox, images)
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut State,
        (hitbox, images): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.styled.layout().clone();
        let before = TextSelection::selected_text(window, cx);
        let projection = state.handle.update_runs(
            &[
                TextSelectionRun::new(self.built.text.clone(), layout.clone(), bounds)
                    .with_document_order(self.order),
            ],
            cx,
        );
        if before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }
        let selected = projection.ranges().first().cloned().flatten();
        *state.selected.borrow_mut() = selected.clone();
        if let Some(range) = selected {
            Self::paint_selection(&layout, range, window);
        }
        self.styled.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
        for image in images {
            image.paint(window, cx);
        }

        // The pointer: a hand over names and links, the text cursor elsewhere.
        let hovered = hitbox
            .is_hovered(window)
            .then(|| self.target_at(&layout, window.mouse_position()))
            .flatten()
            .filter(|&i| !matches!(self.built.targets[i].1, Target::Emote(_)));
        let cursor = if hovered.is_some() {
            CursorStyle::PointingHand
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(cursor, hitbox);

        let targets: Rc<Vec<(Range<usize>, Target)>> = Rc::new(self.built.targets.clone());
        // Each emote with the names of any zero-width emotes on it, for its hover card.
        let emotes: Rc<Vec<EmoteCard>> = Rc::new(
            self.built
                .images
                .iter()
                .map(|i| {
                    let names = self
                        .built
                        .images
                        .iter()
                        .filter(|o| o.at == i.at)
                        .map(|o| o.name.clone())
                        .collect();
                    (names, i.url.clone(), i.size, i.scope)
                })
                .collect(),
        );
        window.on_mouse_event({
            let (hitbox, layout, targets) = (hitbox.clone(), layout.clone(), targets.clone());
            let kept = state.hovered.clone();
            let on_hover = self.on_hover.clone();
            let key = self.key.clone();
            move |e: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                let now = hitbox
                    .is_hovered(window)
                    .then(|| target_index(&layout, &targets, e.position))
                    .flatten();
                if now != kept.get() {
                    kept.set(now);
                    window.refresh();
                    if let Some(f) = &on_hover {
                        let hover = now.and_then(|i| {
                            let (range, target) = &targets[i];
                            let at = layout.position_for_index(range.start).unwrap_or(e.position);
                            match target {
                                Target::Link(url) => Some(Hover::Link {
                                    url: url.clone(),
                                    at,
                                }),
                                Target::Emote(ix) => {
                                    let (names, url, size, scope) = emotes.get(*ix)?.clone();
                                    Some(Hover::Emote {
                                        names,
                                        url,
                                        size,
                                        scope,
                                        at,
                                    })
                                }
                                Target::Name => None,
                            }
                        });
                        f(key.clone(), hover, window, cx);
                    }
                }
            }
        });
        window.on_mouse_event({
            let (hitbox, down) = (hitbox.clone(), state.down.clone());
            move |e: &MouseDownEvent, phase, window, _| {
                if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                    down.set(Some(e.position));
                }
            }
        });
        if let Some(on_click) = self.on_click.clone() {
            let (hitbox, down) = (hitbox.clone(), state.down.clone());
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if !phase.bubble() || e.button != MouseButton::Left {
                    return;
                }
                let Some(from) = down.take() else {
                    return;
                };
                let moved = (e.position.x - from.x).abs() + (e.position.y - from.y).abs();
                if moved > px(4.) || !hitbox.is_hovered(window) {
                    return;
                }
                if let Some(i) = target_index(&layout, &targets, e.position) {
                    on_click(&targets[i].1, e.position, window, cx);
                }
            });
        }
    }
}

fn target_index(
    layout: &TextLayout,
    targets: &[(Range<usize>, Target)],
    position: Point<Pixels>,
) -> Option<usize> {
    let ix = layout.index_for_position(position).ok()?;
    targets
        .iter()
        .position(|(r, _)| r.start <= ix && ix < r.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_web_addresses_without_trailing_punctuation() {
        assert_eq!(
            link_in("https://x.io/a."),
            Some(("https://x.io/a".into(), 14))
        );
        assert_eq!(
            link_in("www.example.com,"),
            Some(("https://www.example.com".into(), 15))
        );
        assert_eq!(
            link_in("https://en.wikipedia.org/wiki/Rust_(language))"),
            Some(("https://en.wikipedia.org/wiki/Rust_(language)".into(), 45))
        );
        assert_eq!(link_in("javascript:alert(1)"), None);
        assert_eq!(link_in("file:///c:/x"), None);
        assert_eq!(link_in("https://"), None);
        assert_eq!(link_in("hello"), None);
    }

    #[test]
    fn copying_turns_placeholders_back_into_names() {
        let mut parts = Parts::default();
        parts.text("Bob: hi ", None);
        parts.emote(
            "Kappa",
            "https://e/1",
            Some((28, 28)),
            false,
            Default::default(),
        );
        parts.text(" there", None);
        let b = build(&parts.0, px(7.));
        let whole = copy_text(&b.text, &b.copies, 0..b.text.len());
        assert_eq!(whole, "Bob: hi Kappa there");
        let slot = b.copies[0].0.clone();
        // Half of the emote's slot still copies its whole name.
        let part = copy_text(&b.text, &b.copies, 5..slot.start + 1);
        assert_eq!(part, "hi Kappa");
        assert_eq!(copy_text(&b.text, &b.copies, 0..3), "Bob");
    }

    #[test]
    fn a_zero_width_emote_takes_no_space() {
        let mut parts = Parts::default();
        parts.emote(
            "Base",
            "https://e/1",
            Some((28, 28)),
            false,
            Default::default(),
        );
        parts.emote(
            "Rain",
            "https://e/2",
            Some((28, 28)),
            true,
            Default::default(),
        );
        let b = build(&parts.0, px(7.));
        assert_eq!(b.images.len(), 2);
        assert_eq!(b.images[0].at, b.images[1].at);
        assert_eq!(b.copies.len(), 1, "copies as the base emote");
    }

    #[test]
    fn linked_text_splits_out_addresses() {
        let mut parts = Parts::default();
        parts.linked("see https://x.io/a. now", None);
        let b = build(&parts.0, px(7.));
        assert_eq!(b.text, "see https://x.io/a. now");
        assert_eq!(b.targets.len(), 1);
        assert_eq!(&b.text[b.targets[0].0.clone()], "https://x.io/a");
        assert_eq!(b.targets[0].1, Target::Link("https://x.io/a".into()));
    }
}
