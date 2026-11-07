#![cfg_attr(windows, windows_subsystem = "windows")]

mod network;
mod resources;
mod ui;
mod window_chrome;

use gpui_kit::*;

fn main() {
    let assets = match resources::initialize() {
        Ok(assets) => assets,
        Err(error) => resources::fatal(error),
    };
    gpui_kit::application().with_assets(assets).run(|cx| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1330.), px(830.)), cx)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("UTAchat");
            cx.new(|cx| ui::UTAchat::new(window, cx))
        }).expect("UTAchat window");
        window_chrome::apply();
        cx.activate(true);
    });
}
