mod alignment;
mod article;
mod controller;
mod devices;
mod font_picker;
mod jog_settings;
mod kuixiang_config;
mod kuixiang_page;
mod kuixiang_selector;
mod locale;
mod screen_output;
mod view;
mod workspace_profile;
mod workspace_wizard;

use controller::{Action, Controller};
use std::path::{Path, PathBuf};
use view::WriterView;
use zgpui::native::{Event, Palette, Theme, Ui, Window, colors};

const HELP: &str = "Zuse Writer · 写字机

zuse-writer                       打开书写页，读取本地奎享配置
zuse-writer --align               打开行程与装笔向导
zuse-writer --version             显示 Writer 与配套 zuse 的版本
zuse-writer --kuixiang-config <native|flatpak|安装或配置目录> [--align]
zuse-writer --show-defaults [--kuixiang-config <来源>]    显示来源、原值与生效值后退出
zuse-writer --kuixiang-page <page_settings.json> [预设名]    本次使用指定纸张和边距";

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
    let mut args = std::env::args_os().skip(1).peekable();
    let mut align = false;
    let mut inspect = false;
    let mut selected = None;
    let mut page = None;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--help" | "-h") => {
                println!("{HELP}");
                return Ok(());
            }
            Some("--version" | "-V") => {
                print!("Zuse Writer {} (zuse {})", zuse::VERSION, zuse::VERSION);
                if let Some(revision) = option_env!("ZUSE_BUILD_REVISION") {
                    print!(" [{revision}]");
                }
                println!();
                return Ok(());
            }
            Some("--align") => align = true,
            Some("--show-defaults") => inspect = true,
            Some("--kuixiang-config") if selected.is_none() => {
                let path = args.next().ok_or(HELP)?;
                selected = Some(kuixiang_config::selected_root(&path, home.as_deref())?);
            }
            Some("--kuixiang-page") if page.is_none() => {
                let path = args.next().ok_or(HELP)?;
                let name = args
                    .next_if(|s| !s.as_encoded_bytes().starts_with(b"--"))
                    .map(|s| s.into_string().map_err(|_| "预设名需要 Unicode 文本"))
                    .transpose()?;
                page = Some((path, name));
            }
            _ => return Err(HELP.into()),
        }
    }
    // The original page-only entry point keeps its typography and motion settings.
    let discovery_home = if page.is_none() {
        home.as_deref()
    } else {
        None
    };
    let mut sources = kuixiang_selector::Sources::from_home(discovery_home);
    let mut source_notice = None;
    let explicit_source = selected.is_some();
    let selected = if page.is_some() && selected.is_none() {
        None
    } else if let Some(path) = selected.as_deref() {
        // An explicit launch path has priority even on systems without a config directory.
        sources.mark_explicit(path);
        Some(path.to_path_buf())
    } else {
        match kuixiang_selector::preference().and_then(|p| sources.saved_root(&p)) {
            Ok(path) => path,
            Err(message) => {
                source_notice = Some(message);
                None
            }
        }
    };
    let mut defaults = match kuixiang_config::discover(selected.as_deref(), discovery_home) {
        Ok(defaults) => defaults,
        Err(error) if !explicit_source && !inspect => {
            let mut fallback = kuixiang_config::discover(None, None)?;
            fallback.summary = format!("读取奎享配置失败，沿用 Writer 默认值：{error}");
            fallback.report = fallback.summary.clone();
            fallback
        }
        Err(error) => return Err(error),
    };
    if let Some((path, name)) = page {
        let preset = kuixiang_page::load(Path::new(&path), name.as_deref())?;
        preset.apply(&mut defaults.form, false)?;
        defaults.summary = format!("已导入纸张 {:?} 和四边页边距。", preset.name);
        defaults.report.push_str(&format!("\n{}", defaults.summary));
    }
    if let Some(message) = source_notice {
        defaults.summary = format!("{message} {}", defaults.summary);
        defaults.report = format!("{message}\n{}", defaults.report);
    }
    let source_menu = kuixiang_selector::Menu {
        sources,
        previous: defaults.form.clone(),
    };
    let mut controller = Controller::with_form(defaults.form);
    controller.jog_source = defaults.source;
    if inspect {
        println!("{}", defaults.report);
        let f = &controller.form;
        println!(
            "生效设置：单次 XY {} mm，XY 点动请求 {} mm/min，书写进给 {} mm/min，书写抬落笔 {} mm/min，波特率 {}，页面 {} × {} mm，四边距 {:?} mm，Writer 字体 {:?}。",
            f.jog_step,
            f.jog_feed,
            f.feed,
            f.lift_feed.unwrap_or(f.feed),
            f.baud,
            f.area_width,
            f.area_height,
            f.margins,
            f.font
        );
        return Ok(());
    }
    if !defaults.summary.is_empty() {
        eprintln!("{}", defaults.report);
    }
    let mut window = Window::new("Zuse Writer · 写字机", 1120, 820)?;
    window.set_min_size(320, 240)?;
    let mut view = WriterView::new(&mut controller, align);
    view.set_sources(source_menu, defaults.summary);
    let mut ui = Ui::default();
    let mut event = None;
    let mut key_read_at: Option<std::time::Instant> = None;
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
            view.calibration_pointer(input, &controller);
            let (_, height) = window.size();
            let fresh =
                key_read_at.is_some_and(|t| t.elapsed() <= std::time::Duration::from_millis(80));
            if !closing && let Some(repaint) = view.calibration_key(input, fresh, &mut controller) {
                redraw |= repaint;
                event = None;
            } else if view.article_input(input, &mut ui, height) {
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
        let wait = if controller.busy() || view.needs_tick() {
            40
        } else {
            200
        };
        // Never turn buffered OS repeats from a slow frame into motion. Other
        // controls still see queued events normally.
        event = window.event(0);
        key_read_at = None;
        if event.is_none() {
            let start = std::time::Instant::now();
            event = window.event(wait);
            if start.elapsed() <= std::time::Duration::from_millis(wait as u64 + 35) {
                key_read_at = Some(std::time::Instant::now());
            }
        }
    }
    Ok(())
}
