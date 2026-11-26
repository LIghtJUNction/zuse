mod controller;
mod view;

use controller::{Action, Controller};
use view::WriterView;
use zgpui::native::{Event, Palette, Theme, Ui, Window};

fn main() -> Result<(), String> {
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
    let mut view = WriterView::new(&mut controller);
    let mut ui = Ui::default();
    let mut event = None;
    let mut redraw = true;
    let mut closing = false;
    loop {
        let before = (
            controller.busy(),
            controller.mpos,
            controller.wpos,
            controller.cancel_target(),
        );
        let status = controller.status.clone();
        let calibration = (controller.form.pen_up, controller.form.pen_down);
        controller.poll();
        view.sync_heights(calibration, &controller);
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
        redraw |= before
            != (
                controller.busy(),
                controller.mpos,
                controller.wpos,
                controller.cancel_target(),
            )
            || status != controller.status;
        match event {
            Some(Event::Scroll(delta)) => view.scroll(delta),
            Some(Event::Move(x, y)) if !ui.pointer_changed(x, y) => event = None,
            _ => {}
        }
        if redraw || event.is_some() {
            let (w, h) = window.size();
            let had_input = event.is_some();
            ui.begin(event.take());
            let mut canvas = window.begin(0xffffffff);
            view.draw(&mut canvas, &mut ui, &mut controller, w, h, closing);
            ui.end();
            drop(canvas);
            window.present();
            if let Some(text) = ui.clipboard.take() {
                window.copy(&text);
            }
            redraw = had_input;
        }
        event = window.event(if controller.busy() { 40 } else { 200 });
    }
    Ok(())
}
