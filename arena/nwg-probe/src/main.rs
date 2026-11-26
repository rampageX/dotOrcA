#![windows_subsystem = "windows"]
use arena_common::{Worker, HINT, START, CLEAR};
use native_windows_gui as nwg;
use native_windows_derive::NwgUi;
use nwg::NativeUi;
use std::time::Duration;

#[derive(Default, NwgUi)]
struct Probe {
    #[nwg_control(size: (720, 400), title: "dotOrcA S00 | NWG", accept_files: true)]
    #[nwg_events(OnWindowClose: [nwg::stop_thread_dispatch()], OnFileDrop: [Probe::drop_files(SELF, EVT_DATA)])]
    window: nwg::Window,
    #[nwg_layout(parent: window, spacing: 8)]
    grid: nwg::GridLayout,
    #[nwg_control(text: "Worker: idle / 后台待命")]
    #[nwg_layout_item(layout: grid, col: 0, row: 0, col_span: 2)]
    status: nwg::Label,
    #[nwg_control(text: HINT, readonly: true)]
    #[nwg_layout_item(layout: grid, col: 0, row: 1, col_span: 2, row_span: 5)]
    output: nwg::TextBox,
    #[nwg_control(text: START)]
    #[nwg_layout_item(layout: grid, col: 0, row: 6)]
    #[nwg_events(OnButtonClick: [Probe::start])]
    run: nwg::Button,
    #[nwg_control(text: CLEAR)]
    #[nwg_layout_item(layout: grid, col: 1, row: 6)]
    #[nwg_events(OnButtonClick: [Probe::clear])]
    clear: nwg::Button,
    #[nwg_control(parent: window, interval: Duration::from_millis(100))]
    #[nwg_events(OnTimerTick: [Probe::tick])]
    timer: nwg::AnimationTimer,
    worker: Worker,
}
impl Probe {
    fn start(&self) { self.worker.start(); self.status.set_text(&self.worker.text()); self.timer.start(); }
    fn tick(&self) { self.status.set_text(&self.worker.text()); if !self.worker.running() { self.timer.stop(); } }
    fn clear(&self) { self.output.set_text(HINT); }
    fn drop_files(&self, data: &nwg::EventData) { self.output.set_text(&data.on_file_drop().files().join("\r\n")); }
}
fn main() {
    nwg::init().expect("NWG initialization");
    nwg::Font::set_global_family("Segoe UI").expect("system font");
    let _ui = Probe::build_ui(Default::default()).expect("NWG window");
    nwg::dispatch_thread_events();
}
