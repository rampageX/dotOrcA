#![windows_subsystem = "windows"]
use arena_common::{Worker, HINT, START, CLEAR};
use winsafe::{self as w, co, gui, prelude::*};

fn main() -> w::AnyResult<()> {
    let worker = Worker::default();
    let wnd = gui::WindowMain::new(gui::WindowMainOpts {
        title: "dotOrcA S00 | WinSafe",
        size: gui::dpi(720, 400),
        style: co::WS::OVERLAPPEDWINDOW | co::WS::VISIBLE | co::WS::CLIPCHILDREN,
        ex_style: co::WS_EX::ACCEPTFILES,
        ..Default::default()
    });
    let status = gui::Label::new(&wnd, gui::LabelOpts {
        text: &worker.text(), position: gui::dpi(12, 12), size: gui::dpi(680, 25), ..Default::default()
    });
    let output = gui::Edit::new(&wnd, gui::EditOpts {
        text: HINT, position: gui::dpi(12, 48), width: gui::dpi_x(690), height: gui::dpi_y(280),
        control_style: co::ES::MULTILINE | co::ES::AUTOVSCROLL | co::ES::READONLY,
        resize_behavior: (gui::Horz::Resize, gui::Vert::Resize), ..Default::default()
    });
    let run = gui::Button::new(&wnd, gui::ButtonOpts {
        text: START, position: gui::dpi(12, 350), size: gui::dpi(220, 30),
        resize_behavior: (gui::Horz::None, gui::Vert::Repos), ..Default::default()
    });
    let clear = gui::Button::new(&wnd, gui::ButtonOpts {
        text: CLEAR, position: gui::dpi(250, 350), size: gui::dpi(120, 30),
        resize_behavior: (gui::Horz::None, gui::Vert::Repos), ..Default::default()
    });
    let (w2, s2, work2) = (wnd.clone(), status.clone(), worker.clone());
    run.on().bn_clicked(move || {
        work2.start();
        s2.hwnd().SetWindowText(&work2.text())?;
        w2.hwnd().SetTimer(1, 100, None)?;
        Ok(())
    });
    let (w2, s2, work2) = (wnd.clone(), status.clone(), worker.clone());
    wnd.on().wm_timer(1, move || {
        s2.hwnd().SetWindowText(&work2.text())?;
        if !work2.running() { w2.hwnd().KillTimer(1)?; }
        Ok(())
    });
    let o2 = output.clone();
    clear.on().bn_clicked(move || { o2.hwnd().SetWindowText(HINT)?; Ok(()) });
    let o2 = output.clone();
    wnd.on().wm_drop_files(move |p| {
        let files = p.hdrop.DragQueryFile()?.collect::<Result<Vec<_>, _>>()?;
        o2.hwnd().SetWindowText(&files.join("\r\n"))?;
        Ok(())
    });
    wnd.run_main(None)?;
    Ok(())
}
