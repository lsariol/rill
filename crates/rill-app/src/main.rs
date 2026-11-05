//! Rill's desktop window. Phase 0 spike: the composer, its outline and Tab cycling.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod composer;
mod style;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, Theme, ThemeMode};

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        composer::init(cx);

        let bounds = Bounds::centered(None, size(px(420.), px(640.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| composer::ChatWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        });
        if let Err(err) = opened {
            eprintln!("✗ GPUI: could not open the window: {err}. Check the graphics driver.");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
