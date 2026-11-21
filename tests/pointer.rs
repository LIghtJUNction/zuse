#![cfg(feature = "std")]

use zuse::{
    motion::{Curve, MAX_COORDINATE, Point},
    pointer::{MoveOptions, MovePlan},
};

fn point(x: f64, y: f64) -> Point {
    Point { x, y }
}

#[test]
fn default_plan_keeps_current_start_and_target_offset() {
    let options = MoveOptions {
        offset: point(-4.0, 9.0),
        ..MoveOptions::default()
    };
    let plan = MovePlan::new(point(-200.0, -80.0), point(40.0, -20.0), &options).unwrap();
    assert_eq!(plan.from(), point(-200.0, -80.0));
    assert_eq!(plan.to(), point(36.0, -11.0));
    assert_eq!(plan.samples()[0].point, plan.from());
    assert_eq!(plan.samples().last().unwrap().point, plan.to());
    assert_eq!(plan.duration_ms(), 350);
    assert_eq!(plan.samples().last().unwrap().at_ms, 350);
    assert!(plan.samples().windows(2).all(|p| p[0].at_ms < p[1].at_ms));
}

#[test]
fn default_target_motion_eases_with_small_jitter_and_exact_endpoints() {
    let plan = MovePlan::new(point(0.0, 0.0), point(200.0, 0.0), &MoveOptions::default()).unwrap();
    let samples = plan.samples();
    assert_eq!(plan.duration_ms(), 350);
    assert_eq!(samples.first().unwrap().point, point(0.0, 0.0));
    assert_eq!(samples.last().unwrap().point, point(200.0, 0.0));
    assert!(samples.iter().any(|s| s.point.y.abs() > 0.01));
    assert!(samples.iter().all(|s| s.point.y.abs() <= 0.6));
    let first = samples[1].point.x - samples[0].point.x;
    let middle = samples[samples.len() / 2 + 1].point.x - samples[samples.len() / 2].point.x;
    assert!(
        middle > first * 2.0,
        "default motion must accelerate rather than move at a constant rate"
    );
}

#[test]
fn piecewise_geometry_uses_absolute_waypoints_and_ignores_duplicate_knots() {
    let options = MoveOptions {
        duration_ms: Some(400),
        steps: Some(4),
        curve: Curve::Linear,
        jitter: 0.0,
        via: vec![point(-40.0, 0.0), point(0.0, 0.0), point(0.0, 0.0)],
        ..MoveOptions::default()
    };
    let plan = MovePlan::new(point(-40.0, 0.0), point(0.0, 40.0), &options).unwrap();
    let points: Vec<_> = plan.samples().iter().map(|s| s.point).collect();
    assert_eq!(
        points,
        vec![
            point(-40.0, 0.0),
            point(-20.0, 0.0),
            point(0.0, 0.0),
            point(0.0, 20.0),
            point(0.0, 40.0),
        ]
    );
    assert_eq!(plan.length(), 80.0);
}

#[test]
fn jitter_and_speed_variation_are_reproducible_and_preserve_endpoints() {
    let options = MoveOptions {
        jitter: 5.0,
        speed_variation: 1.0,
        seed: 42,
        ..MoveOptions::default()
    };
    let from = point(-25.0, 0.0);
    let to = point(100.0, 0.0);
    let plan = MovePlan::new(from, to, &options).unwrap();
    assert_eq!(plan, MovePlan::new(from, to, &options).unwrap());
    let other = MovePlan::new(
        from,
        to,
        &MoveOptions {
            seed: 43,
            ..options
        },
    )
    .unwrap();
    assert_ne!(plan.samples(), other.samples());
    for sample in plan.samples() {
        assert!(sample.point.y.abs() <= 5.0);
    }
    assert_eq!(plan.samples().first().unwrap().point, from);
    assert_eq!(plan.samples().last().unwrap().point, to);
}

#[test]
fn average_speed_uses_the_whole_sampled_route() {
    let options = MoveOptions {
        speed: Some(100.0),
        curve: Curve::Linear,
        jitter: 0.0,
        ..MoveOptions::default()
    };
    let from = point(0.0, 0.0);
    let to = point(100.0, 0.0);
    let direct = MovePlan::new(from, to, &options).unwrap();
    let detour = MovePlan::new(
        from,
        to,
        &MoveOptions {
            via: vec![point(50.0, 100.0)],
            ..options
        },
    )
    .unwrap();
    assert_eq!(direct.duration_ms(), 1000);
    assert!(detour.duration_ms() > 2200);
    assert_eq!(detour.duration_ms(), (detour.length() * 10.0).ceil() as u32);
    assert_eq!(detour.samples().last().unwrap().at_ms, detour.duration_ms());
}

#[test]
fn fixed_duration_does_not_change_for_a_longer_route() {
    let options = MoveOptions {
        duration_ms: Some(500),
        ..MoveOptions::default()
    };
    let from = point(0.0, 0.0);
    let to = point(100.0, 0.0);
    let direct = MovePlan::new(from, to, &options).unwrap();
    let detour = MovePlan::new(
        from,
        to,
        &MoveOptions {
            via: vec![point(50.0, 100.0)],
            ..options
        },
    )
    .unwrap();
    assert_eq!(direct.duration_ms(), 500);
    assert_eq!(detour.duration_ms(), 500);
    assert!(detour.length() > direct.length());
}

#[test]
fn curve_and_seeded_time_warp_control_the_path() {
    let base = MoveOptions {
        steps: Some(4),
        curve: Curve::Linear,
        jitter: 0.0,
        ..MoveOptions::default()
    };
    let from = point(0.0, 0.0);
    let to = point(100.0, 0.0);
    let linear = MovePlan::new(from, to, &base).unwrap();
    let bezier = MovePlan::new(
        from,
        to,
        &MoveOptions {
            curve: Curve::Bezier,
            ..base.clone()
        },
    )
    .unwrap();
    let eased = MovePlan::new(
        from,
        to,
        &MoveOptions {
            curve: Curve::EaseInOut,
            ..base.clone()
        },
    )
    .unwrap();
    let variable = MovePlan::new(
        from,
        to,
        &MoveOptions {
            speed_variation: 1.0,
            ..base
        },
    )
    .unwrap();
    assert_eq!(linear.samples()[2].point, point(50.0, 0.0));
    assert_eq!(bezier.samples()[2].point, point(50.0, 15.0));
    assert!(eased.samples()[1].point.x < linear.samples()[1].point.x);
    assert_ne!(variable.samples()[2].point.x, linear.samples()[2].point.x);
    assert!(
        variable
            .samples()
            .windows(2)
            .all(|p| p[0].point.x < p[1].point.x)
    );
}

#[test]
fn fast_speed_uses_non_decreasing_millisecond_times() {
    let plan = MovePlan::new(
        point(0.0, 0.0),
        point(1.0, 0.0),
        &MoveOptions {
            speed: Some(1_000_000.0),
            ..MoveOptions::default()
        },
    )
    .unwrap();
    assert_eq!(plan.duration_ms(), 1);
    assert_eq!(plan.samples().last().unwrap().at_ms, 1);
    assert!(plan.samples().windows(2).all(|p| p[0].at_ms <= p[1].at_ms));
}

#[test]
fn invalid_parameters_fail_before_sampling() {
    for options in [
        MoveOptions {
            duration_ms: Some(10),
            speed: Some(10.0),
            ..MoveOptions::default()
        },
        MoveOptions {
            duration_ms: Some(60_001),
            ..MoveOptions::default()
        },
        MoveOptions {
            duration_ms: Some(0),
            steps: Some(2),
            ..MoveOptions::default()
        },
        MoveOptions {
            duration_ms: Some(1),
            steps: Some(2),
            ..MoveOptions::default()
        },
        MoveOptions {
            speed: Some(0.0),
            ..MoveOptions::default()
        },
        MoveOptions {
            speed: Some(-1.0),
            ..MoveOptions::default()
        },
        MoveOptions {
            speed: Some(f64::NAN),
            ..MoveOptions::default()
        },
        MoveOptions {
            speed: Some(f64::INFINITY),
            ..MoveOptions::default()
        },
        MoveOptions {
            steps: Some(0),
            ..MoveOptions::default()
        },
        MoveOptions {
            steps: Some(4097),
            ..MoveOptions::default()
        },
        MoveOptions {
            jitter: -1.0,
            ..MoveOptions::default()
        },
        MoveOptions {
            jitter: 1001.0,
            ..MoveOptions::default()
        },
        MoveOptions {
            jitter: f64::NAN,
            ..MoveOptions::default()
        },
        MoveOptions {
            speed_variation: 1.1,
            ..MoveOptions::default()
        },
        MoveOptions {
            speed_variation: f64::NAN,
            ..MoveOptions::default()
        },
        MoveOptions {
            offset: point(f64::INFINITY, 0.0),
            ..MoveOptions::default()
        },
        MoveOptions {
            via: vec![Point::default(); 65],
            ..MoveOptions::default()
        },
        MoveOptions {
            via: vec![point(MAX_COORDINATE + 1.0, 0.0)],
            ..MoveOptions::default()
        },
    ] {
        assert!(options.validate().is_err(), "{options:?}");
        let mut called = false;
        assert!(
            MovePlan::with_path(Point::default(), Point::default(), &options, |_, _, _| {
                called = true;
                Point::default()
            })
            .is_err()
        );
        assert!(!called);
    }
}

#[test]
fn callback_is_sampled_once_and_receives_current_start_and_offset_target() {
    let options = MoveOptions {
        speed: Some(100.0),
        steps: Some(4),
        curve: Curve::Linear,
        offset: point(0.0, 10.0),
        jitter: 0.0,
        ..MoveOptions::default()
    };
    let mut seen = Vec::new();
    let plan = MovePlan::with_path(
        point(-50.0, -10.0),
        point(50.0, -20.0),
        &options,
        |u, a, b| {
            seen.push((u, a, b));
            if u == 0.0 {
                a
            } else if u == 1.0 {
                b
            } else {
                point(a.x + (b.x - a.x) * u, a.y + 40.0 * u * (1.0 - u))
            }
        },
    )
    .unwrap();
    assert_eq!(seen.len(), 5);
    assert!(
        seen.iter()
            .all(|(_, a, b)| *a == point(-50.0, -10.0) && *b == point(50.0, -10.0))
    );
    assert_eq!(plan.samples()[2].point, point(0.0, 0.0));
    assert_eq!(plan.to(), point(50.0, -10.0));
}

#[test]
fn callback_errors_and_bad_endpoints_never_produce_a_partial_plan() {
    let from = point(0.0, 0.0);
    let to = point(100.0, 0.0);
    let options = MoveOptions {
        steps: Some(4),
        curve: Curve::Linear,
        ..MoveOptions::default()
    };
    for bad in [point(f64::NAN, 0.0), point(MAX_COORDINATE + 1.0, 0.0)] {
        assert!(
            MovePlan::with_path(from, to, &options, |u, a, b| {
                if u == 0.0 {
                    a
                } else if u == 1.0 {
                    b
                } else {
                    bad
                }
            })
            .is_err()
        );
    }
    assert!(MovePlan::with_path(from, to, &options, |_, _, _| to).is_err());
    assert!(MovePlan::with_path(from, to, &options, |_, _, _| from).is_err());
    assert!(
        MovePlan::with_path(
            from,
            to,
            &MoveOptions {
                via: vec![point(50.0, 0.0)],
                ..options
            },
            |u, _, _| point(100.0 * u, 0.0)
        )
        .is_err()
    );
}

#[test]
fn coincident_endpoints_and_immediate_paths_remain_finite() {
    let at = point(-10.0, 20.0);
    let options = MoveOptions {
        via: vec![at, at],
        jitter: 0.0,
        ..MoveOptions::default()
    };
    let stationary = MovePlan::new(at, at, &options).unwrap();
    assert_eq!(stationary.length(), 0.0);
    assert!(stationary.samples().iter().all(|s| s.point == at));
    let instant = MovePlan::new(
        at,
        point(100.0, 30.0),
        &MoveOptions {
            duration_ms: Some(0),
            ..MoveOptions::default()
        },
    )
    .unwrap();
    assert_eq!(instant.samples().len(), 2);
    assert!(instant.samples().iter().all(|s| s.at_ms == 0));
    let stationary_speed = MovePlan::new(
        at,
        at,
        &MoveOptions {
            speed: Some(100.0),
            jitter: 0.0,
            ..MoveOptions::default()
        },
    )
    .unwrap();
    assert_eq!(stationary_speed.duration_ms(), 0);
    assert_eq!(stationary_speed.length(), 0.0);
}

#[test]
fn resolved_overflow_and_excessive_speed_duration_are_rejected() {
    assert!(
        MovePlan::new(
            Point::default(),
            point(MAX_COORDINATE, 0.0),
            &MoveOptions {
                offset: point(1.0, 0.0),
                ..MoveOptions::default()
            }
        )
        .is_err()
    );
    assert!(
        MovePlan::new(
            point(f64::NAN, 0.0),
            Point::default(),
            &MoveOptions::default()
        )
        .is_err()
    );
    assert!(
        MovePlan::new(
            Point::default(),
            point(100.0, 0.0),
            &MoveOptions {
                speed: Some(0.001),
                ..MoveOptions::default()
            }
        )
        .is_err()
    );
}
