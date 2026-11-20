//! Preview a programmatic path by default. Pass --execute BUTTON_TEXT to
//! explicitly locate a button and move the real desktop pointer.

#[cfg(feature = "std")]
use zuse::{
    motion::Point,
    pointer::{MoveOptions, MovePlan},
    runtime::Runtime,
    target::TargetQuery,
};

#[cfg(feature = "std")]
fn arc(u: f64, from: Point, to: Point) -> Point {
    if u == 0.0 {
        return from;
    }
    if u == 1.0 {
        return to;
    }
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let bow = 0.5 * u * (1.0 - u);
    Point {
        x: from.x + dx * u - dy * bow,
        y: from.y + dy * u + dx * bow,
    }
}

#[cfg(feature = "std")]
fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let button = match args.next().as_deref() {
        None => None,
        Some("--execute") => {
            let name = args.next().ok_or("--execute requires a button name")?;
            if args.next().is_some() {
                return Err("usage: target_motion [--execute BUTTON_TEXT]".into());
            }
            Some(name)
        }
        Some("--help") => {
            println!(
                "target_motion [--execute BUTTON_TEXT]\nDefault: preview a path without desktop access."
            );
            return Ok(());
        }
        Some(_) => return Err("usage: target_motion [--execute BUTTON_TEXT]".into()),
    };
    let options = MoveOptions {
        duration_ms: Some(650),
        jitter: 1.2,
        speed_variation: 0.25,
        seed: 42,
        offset: Point { x: 4.0, y: -3.0 },
        ..MoveOptions::default()
    };
    if let Some(button) = button {
        let query = TargetQuery::button(&button);
        let receipt = Runtime::new().move_to_with(&query, &options, arc)?;
        println!("{}", receipt.to_json());
    } else {
        let plan = MovePlan::with_path(
            Point { x: 80.0, y: 200.0 },
            Point { x: 420.0, y: 160.0 },
            &options,
            arc,
        )?;
        println!(
            "{{\"mode\":\"plan\",\"from\":{{\"x\":{},\"y\":{}}},\"to\":{{\"x\":{},\"y\":{}}},\"duration_ms\":{},\"samples\":{},\"path_length\":{}}}",
            plan.from().x,
            plan.from().y,
            plan.to().x,
            plan.to().y,
            plan.duration_ms(),
            plan.samples().len(),
            plan.length(),
        );
    }
    Ok(())
}

#[cfg(feature = "std")]
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "std"))]
fn main() {
    eprintln!("target_motion requires the std feature");
    std::process::exit(1);
}
