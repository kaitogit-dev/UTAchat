mod network;
mod ui;

use gpui_kit::*;
use gpui_kit::assets::Assets;

fn main() {
    gpui_kit::application().with_assets(Assets).run(|cx| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1330.), px(830.)), cx)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("UTAchat");
            cx.new(|cx| ui::UTAchat::new(window, cx))
        }).expect("UTAchat window");
        cx.activate(true);
    });
}
