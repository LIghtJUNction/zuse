mod alignment;
mod controller;
mod devices;
mod font_picker;
mod screen_output;
mod view;
mod workspace_profile;
mod workspace_wizard;

use controller::{Action, Controller};
use view::WriterView;
use zgpui::native::{Event, Palette, Theme, Ui, Window};

const HELP: &str = "Zuse Writer · 写字机\n\nzuse-writer          打开书写页\nzuse-writer --align  打开行程与装笔向导，记录机台范围和纸面高度";
fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let align = match (args.next(), args.next()) {
        (None, None) => false,
        (Some(flag), None) if flag == "--align" => true,
        (Some(flag), None) if flag == "--help" || flag == "-h" => {
            println!("{HELP}");
            return Ok(());
        }
        _ => return Err(HELP.into()),
    };
    let mut window = Window::new("Zuse Writer · 写字机", 1120, 820)?;
    let mut palette = Palette::for_theme(Theme::Light);
    palette.accent = 0x252525ff;
    palette.accent_hover = 0x444444ff;
    palette.accent_soft = 0xececeaff;
    palette.hover = 0xf4f4f2ff;
    palette.line = 0xdededdff;
    palette.text = 0x222222ff;
    palette.muted = 0x686865ff;
    window.set_palette(palette);

    let mut controller = Controller::default();
    let mut view = WriterView::new(&mut controller, align);
    let mut ui = Ui::default();
    let mut event = None;
    let mut redraw = true;
    let mut closing = false;
    let mut initial_preview = !align;
    loop {
        let before = (
            controller.busy(),
            controller.mpos,
            controller.wpos,
            controller.cancel_target(),
        );
        let status = controller.status.clone();
        let previous_baud = controller.form.baud;
        let calibration = (controller.form.pen_up, controller.form.pen_down);
        controller.poll();
        view.sync_heights(calibration, &controller);
        if previous_baud != controller.form.baud {
            view.sync_baud(controller.form.baud);
            redraw = true;
        }
        if closing && !controller.owns_job() {
            break;
        }
        if matches!(event, Some(Event::Close)) {
            if !controller.owns_job() {
                break;
            }
            closing = true;
            controller.dispatch(Action::Cancel);
            event = None;
        }
        if initial_preview && !view.aligning() && !closing && !controller.busy() {
            initial_preview = false;
            controller.dispatch(Action::Preview);
        }
        redraw |= view.needs_tick()
            || before
                != (
                    controller.busy(),
                    controller.mpos,
                    controller.wpos,
                    controller.cancel_target(),
                )
            || status != controller.status;
        match event {
            Some(Event::Key { code: 27, .. })
                if view.aligning() && view.can_navigate() && !controller.busy() =>
            {
                view.back();
                event = None;
                redraw = true;
            }
            Some(Event::Scroll(delta)) => view.scroll(delta),
            Some(Event::Move(x, y)) if !ui.pointer_changed(x, y) => event = None,
            _ => {}
        }
        if redraw || event.is_some() {
            let (w, h) = window.size();
            let had_input = event.is_some();
            let previous_screen = view.screen();
            ui.begin(event.take());
            let mut canvas = window.begin(0xffffffff);
            view.draw(&mut canvas, &mut ui, &mut controller, w, h, closing);
            ui.end();
            drop(canvas);
            window.present();
            if let Some(text) = ui.clipboard.take() {
                window.copy(&text);
            }
            redraw = had_input || previous_screen != view.screen();
        }
        event = window.event(if controller.busy() || view.needs_tick() {
            40
        } else {
            200
        });
    }
    Ok(())
}
