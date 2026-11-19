//! Repeatable core throughput measurements; no platform input is generated.
use std::hint::black_box;
use std::time::Instant;
use zuse::language::Parser;
use zuse::motion::{Curve, Motion, Point};
fn main() {
    let iterations = std::env::var("ZUSE_BENCH_ITERATIONS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(100_000)
        .clamp(1, 1_000_000);
    let source = r#"mouse.move(x=400, y=300, duration=250, curve="smooth", jitter=1, seed=42); keyboard.type(text="hello"); wait(ms=10)"#;
    let start = Instant::now();
    for _ in 0..iterations {
        for call in Parser::new(black_box(source)) {
            black_box(call.unwrap());
        }
    }
    let parse_ns = start.elapsed().as_nanos();
    let motion_iterations = (iterations / 10).max(1);
    let start = Instant::now();
    for seed in 0..motion_iterations {
        for sample in Motion::new(
            Point::default(),
            Point {
                x: 1000.0,
                y: 500.0,
            },
            500,
            64,
            Curve::Bezier,
            1.0,
            seed,
        )
        .unwrap()
        {
            black_box(sample);
        }
    }
    let motion_ns = start.elapsed().as_nanos();
    println!(
        "{{\"script_bytes\":{},\"parse_calls\":{},\"parse_ns\":{},\"ns_per_call\":{},\"motion_samples\":{},\"motion_ns\":{},\"ns_per_sample\":{}}}",
        source.len(),
        iterations * 3,
        parse_ns,
        parse_ns / u128::from(iterations * 3),
        motion_iterations * 65,
        motion_ns,
        motion_ns / u128::from(motion_iterations * 65)
    );
}
