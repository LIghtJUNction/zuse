use zuse::motion::{Curve, FunctionalMotion, MotionError, PathOptions, Point, Sample};

fn samples(options: PathOptions) -> Vec<Sample> {
    FunctionalMotion::new(
        |t| Point {
            x: t * 100.0,
            y: 40.0,
        },
        options,
    )
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

#[test]
fn functional_path_ends_exactly_and_timestamps_rise() {
    let opts = PathOptions {
        duration_ms: 1000,
        steps: 120,
        jitter: 4.0,
        speed_variation: 1.0,
        seed: 7,
        ..PathOptions::default()
    };
    let s = samples(opts);
    assert_eq!(s.len(), 121);
    assert_eq!(
        s[0],
        Sample {
            at_ms: 0,
            point: Point { x: 0.0, y: 40.0 }
        }
    );
    assert_eq!(
        s[120],
        Sample {
            at_ms: 1000,
            point: Point { x: 100.0, y: 40.0 }
        }
    );
    assert!(s.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
    assert!(s.iter().all(|v| (v.point.y - 40.0).abs() <= 4.0));
    assert_eq!(s, samples(opts));
    assert_ne!(s, samples(PathOptions { seed: 8, ..opts }));
}

#[test]
fn seeded_speed_changes_rate_without_reversing_progress() {
    let opt = PathOptions {
        easing: Curve::Linear,
        speed_variation: 1.0,
        ..PathOptions::default()
    };
    let s = samples(opt);
    assert!(s.windows(2).all(|w| w[0].point.x < w[1].point.x));
    assert_ne!(s[60].point.x, 50.0);
    assert_ne!(s, samples(PathOptions { seed: 2, ..opt }));
    assert_eq!(
        samples(PathOptions {
            speed_variation: 0.0,
            ..opt
        })[60]
            .point
            .x,
        50.0
    );
}

#[test]
fn invalid_samples_stop_and_bad_options_fail() {
    let opts = PathOptions::default();
    let mut it = FunctionalMotion::new(
        |t| {
            if t == 0.0 {
                Point::default()
            } else {
                Point {
                    x: f64::NAN,
                    y: 0.0,
                }
            }
        },
        opts,
    )
    .unwrap();
    assert!(it.next().unwrap().is_ok());
    assert_eq!(it.next(), Some(Err(MotionError::NonFinite)));
    assert_eq!(it.next(), None);
    assert!(matches!(
        FunctionalMotion::new(
            |_| Point::default(),
            PathOptions {
                speed_variation: 1.01,
                ..opts
            }
        ),
        Err(MotionError::SpeedRange)
    ));
}
