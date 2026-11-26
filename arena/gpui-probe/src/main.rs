#![windows_subsystem = "windows"]
use arena_common::{Worker, HINT, START, CLEAR};
use gpui_kit::{*, prelude::*, component::{button::*, label::Label, *}};

#[derive(Default)]
struct Probe {
    worker: Worker,
    paths: Vec<String>,
}
impl Render for Probe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.worker.running() { window.request_animation_frame(); }
        let paths = if self.paths.is_empty() { HINT.to_string() } else { self.paths.join("\n") };
        div().id("root-probe").size_full().v_flex().gap_2().p_3()
            .bg(rgb(0x202124)).text_color(rgb(0xe6e6e6))
            .on_drop(cx.listener(|this, files: &ExternalPaths, _, cx| {
                this.paths = files.paths().iter().map(|p| p.to_string_lossy().into_owned()).collect();
                cx.notify();
            }))
            .child(Label::new(self.worker.text()))
            .child(div().id("paths").flex_1().overflow_y_scroll().child(paths))
            .child(div().h_flex().gap_2()
                .child(Button::new("start").primary().label(START).on_click(cx.listener(|this, _, _, cx| {
                    this.worker.start(); cx.notify();
                })))
                .child(Button::new("clear").label(CLEAR).on_click(cx.listener(|this, _, _, cx| {
                    this.paths.clear(); cx.notify();
                }))))
    }
}
fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        gpui_kit::open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(720.), px(400.)), cx))),
            ..Default::default()
        }, cx, |_, cx| cx.new(|_| Probe::default())).expect("GPUI window");
        cx.activate(true);
    });
}
