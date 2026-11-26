mod alignment;
mod article;
mod controller;
mod devices;
mod font_picker;
mod locale;
mod screen_output;
mod view;
mod workspace_profile;
mod workspace_wizard;

use controller::{Action, Controller};
use view::WriterView;
use zgpui::native::{Event, Palette, Theme, Ui, Window, colors};

const HELP: &str = "Zuse Writer · 写字机\n\nzuse-writer          打开书写页\nzuse-writer --align  打开行程与装笔向导，记录机台范围和纸面高度";
fn writer_palette(dark: bool) -> Palette {
    let mut p = Palette::for_theme(if dark { Theme::Dark } else { Theme::Light });
    if dark {
        p.background = 0x191d23ff;
        p.surface = 0x23282fff;
        p.text = 0xe6ebf2ff;
        p.muted = 0xa8b4c2ff;
        p.line = 0x3b434dff;
        p.accent = 0x9bbdffff;
        p.accent_hover = 0xb8d0ffff;
        p.accent_soft = 0x293e5dff;
        p.hover = 0x303841ff;
        p.on_accent = 0x142a46ff;
        p.disabled_surface = 0x2b3139ff;
        p.disabled_text = 0x88929fff;
    } else {
        p.background = 0xf3f5f8ff;
        p.surface = 0xffffffff;
        p.text = 0x212832ff;
        p.muted = 0x586472ff;
        p.line = 0xdce2e9ff;
        p.accent = 0x285fc7ff;
        p.accent_hover = 0x1c4faeff;
        p.accent_soft = 0xe7efffff;
        p.hover = 0xebeff5ff;
        p.disabled_surface = 0xedf0f4ff;
        p.disabled_text = 0x788391ff;
    }
    p
}

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
    window.set_min_size(320, 240)?;
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
        if let Some(input) = event.as_ref() {
            let (_, height) = window.size();
            if view.article_input(input, &mut ui, height) {
                event = None;
                redraw = true;
            }
        }
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
            window.set_palette(writer_palette(view.dark()));
            let mut canvas = window.begin(colors::BG);
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
