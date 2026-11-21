#![cfg(feature = "std")]

use zuse::visual_timeline::{
    EventKind, Frame, PixelFormat, Region, SvgOptions, TimelineOptions, VisualTimeline,
};

fn timeline(width: u32, height: u32, columns: u16, rows: u16) -> VisualTimeline {
    VisualTimeline::new(TimelineOptions::new(
        Region {
            x: 0,
            y: 0,
            width,
            height,
        },
        columns,
        rows,
    ))
    .unwrap()
}
fn push(timeline: &mut VisualTimeline, time: u64, width: u32, height: u32, pixels: &[u8]) {
    timeline
        .push_frame(time, Frame::gray(width, height, pixels).unwrap())
        .unwrap();
}

#[test]
fn same_cell_disappears_and_reappears_as_two_episodes() {
    let mut timeline = timeline(4, 4, 2, 2);
    let baseline = [0; 16];
    let changed = [255, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    push(&mut timeline, 0, 4, 4, &baseline);
    push(&mut timeline, 10, 4, 4, &changed);
    push(&mut timeline, 20, 4, 4, &changed);
    push(&mut timeline, 30, 4, 4, &baseline);
    push(&mut timeline, 40, 4, 4, &changed);
    let snapshot = timeline.snapshot();
    assert_eq!(snapshot.total_episodes, 2);
    assert_eq!(
        snapshot
            .episodes
            .iter()
            .map(|episode| episode.cell)
            .collect::<Vec<_>>(),
        [0, 0]
    );
    assert_eq!(snapshot.episodes[0].appeared_ms, 10);
    assert_eq!(snapshot.episodes[0].last_seen_ms, 20);
    assert_eq!(snapshot.episodes[0].disappeared_ms, Some(30));
    assert_eq!(snapshot.episodes[0].duration_ms, 20);
    assert_eq!(snapshot.episodes[1].appeared_ms, 40);
    assert!(snapshot.episodes[1].is_active());
    let events = timeline.take_events().events;
    assert_eq!(
        events.iter().map(|event| event.kind).collect::<Vec<_>>(),
        [
            EventKind::Appeared,
            EventKind::Disappeared,
            EventKind::Appeared
        ]
    );
    assert_eq!(events[0].episode.id, events[1].episode.id);
    assert_ne!(events[1].episode.id, events[2].episode.id);
}

#[test]
fn twelve_continuous_frames_are_one_episode_with_an_immediate_first_event() {
    let mut timeline = timeline(2, 2, 1, 1);
    push(&mut timeline, 0, 2, 2, &[0; 4]);
    let update = timeline
        .push_frame(10, Frame::gray(2, 2, &[255; 4]).unwrap())
        .unwrap();
    assert_eq!((update.started, update.finished, update.active), (1, 0, 1));
    let first = timeline.take_events();
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].kind, EventKind::Appeared);
    assert_eq!(first.events[0].at_ms, 10);
    assert_eq!(first.events[0].episode.duration_ms, 0);
    assert!(first.events[0].episode.is_active());
    for frame in 2..=12 {
        push(&mut timeline, frame * 10, 2, 2, &[255; 4]);
    }
    assert_eq!(
        timeline.pending_events(),
        0,
        "continuing visibility is not a new appearance"
    );
    assert_eq!(timeline.snapshot().total_episodes, 1);
    push(&mut timeline, 130, 2, 2, &[0; 4]);
    let final_event = timeline.take_events().events;
    assert_eq!(final_event.len(), 1);
    assert_eq!(final_event[0].kind, EventKind::Disappeared);
    assert_eq!(final_event[0].episode.visible_frames, 12);
    assert_eq!(final_event[0].episode.duration_ms, 120);
}

#[test]
fn threshold_hysteresis_keeps_a_continuous_episode_until_the_exit_boundary() {
    let mut timeline = timeline(1, 1, 1, 1);
    for (time, value) in [
        (0, 0),
        (10, 30),
        (20, 20),
        (30, 9),
        (40, 8),
        (50, 12),
        (60, 24),
    ] {
        push(&mut timeline, time, 1, 1, &[value]);
    }
    let snapshot = timeline.snapshot();
    assert_eq!(snapshot.total_episodes, 2);
    assert_eq!(snapshot.episodes[0].visible_frames, 3);
    assert_eq!(snapshot.episodes[0].duration_ms, 30);
    assert_eq!(snapshot.episodes[0].disappeared_ms, Some(40));
    assert_eq!(snapshot.episodes[1].appeared_ms, 60);
}

#[test]
fn reversed_time_changed_dimensions_and_changed_format_leave_state_unchanged() {
    let mut timeline = timeline(2, 2, 1, 1);
    push(&mut timeline, 100, 2, 2, &[0; 4]);
    push(&mut timeline, 110, 2, 2, &[255; 4]);
    let snapshot = timeline.snapshot();
    let pending = timeline.pending_events();
    assert!(
        timeline
            .push_frame(109, Frame::gray(2, 2, &[0; 4]).unwrap())
            .unwrap_err()
            .contains("monotonic")
    );
    assert!(
        timeline
            .push_frame(120, Frame::gray(3, 2, &[0; 6]).unwrap())
            .unwrap_err()
            .contains("dimensions")
    );
    assert!(
        timeline
            .push_frame(120, Frame::rgba(2, 2, &[0; 16]).unwrap())
            .unwrap_err()
            .contains("format")
    );
    assert_eq!(timeline.snapshot(), snapshot);
    assert_eq!(timeline.pending_events(), pending);
    push(&mut timeline, 120, 2, 2, &[0; 4]);
    assert_eq!(timeline.snapshot().episodes[0].duration_ms, 10);
}

#[test]
fn only_the_fixed_region_is_observed_and_row_padding_is_ignored() {
    let options = TimelineOptions::new(
        Region {
            x: 2,
            y: 1,
            width: 2,
            height: 2,
        },
        2,
        1,
    );
    let mut timeline = VisualTimeline::new(options).unwrap();
    let baseline = [0; 32];
    timeline
        .push_frame(
            0,
            Frame::new(6, 4, 8, PixelFormat::Gray8, &baseline).unwrap(),
        )
        .unwrap();
    assert_eq!(timeline.baseline_bytes(), 4);
    let mut outside = [255; 32];
    for y in 1..3 {
        outside[y * 8 + 2..y * 8 + 4].fill(0);
    }
    timeline
        .push_frame(
            10,
            Frame::new(6, 4, 8, PixelFormat::Gray8, &outside).unwrap(),
        )
        .unwrap();
    assert_eq!(timeline.snapshot().total_episodes, 0);
    outside[11] = 255;
    outside[19] = 255;
    timeline
        .push_frame(
            20,
            Frame::new(6, 4, 8, PixelFormat::Gray8, &outside).unwrap(),
        )
        .unwrap();
    let episodes = timeline.snapshot().episodes;
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].cell, 1);
    assert_eq!(
        episodes[0].bounds,
        Region {
            x: 3,
            y: 1,
            width: 1,
            height: 2
        }
    );
}

#[test]
fn rgba_color_changes_are_visible_but_transparent_rgb_is_not() {
    let mut opaque = timeline(1, 1, 1, 1);
    opaque
        .push_frame(0, Frame::rgba(1, 1, &[255, 0, 0, 255]).unwrap())
        .unwrap();
    opaque
        .push_frame(1, Frame::rgba(1, 1, &[0, 255, 0, 255]).unwrap())
        .unwrap();
    assert_eq!(opaque.take_events().events[0].episode.peak_score, 255);
    let mut transparent = timeline(1, 1, 1, 1);
    transparent
        .push_frame(0, Frame::rgba(1, 1, &[255, 0, 0, 0]).unwrap())
        .unwrap();
    transparent
        .push_frame(1, Frame::rgba(1, 1, &[0, 255, 0, 0]).unwrap())
        .unwrap();
    assert_eq!(transparent.snapshot().total_episodes, 0);
}

#[test]
fn history_and_event_retention_are_bounded_and_discard_counts_are_reported() {
    let mut options = TimelineOptions::new(
        Region {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        1,
        1,
    );
    options.completed_capacity = 1;
    options.event_capacity = 1;
    let mut timeline = VisualTimeline::new(options).unwrap();
    push(&mut timeline, 0, 1, 1, &[0]);
    for time in 1..=6 {
        push(
            &mut timeline,
            time,
            1,
            1,
            &[if time % 2 == 1 { 255 } else { 0 }],
        );
    }
    let snapshot = timeline.snapshot();
    assert_eq!(snapshot.total_episodes, 3);
    assert_eq!(snapshot.episodes.len(), 1);
    assert_eq!(snapshot.episodes[0].id, 3);
    assert_eq!(snapshot.discarded_completed_episodes, 2);
    assert_eq!(snapshot.discarded_events, 5);
    assert_eq!(timeline.pending_events(), 1);
    let batch = timeline.take_events();
    assert_eq!(batch.discarded_events, 5);
    assert_eq!(batch.events[0].kind, EventKind::Disappeared);
    assert_eq!(timeline.take_events().discarded_events, 0);
    assert!(snapshot.to_json().contains("\"discarded_events\":5"));
}

#[test]
fn odd_sized_grid_covers_every_scoped_pixel_once_in_row_major_order() {
    let mut timeline = timeline(5, 3, 2, 2);
    push(&mut timeline, 0, 5, 3, &[0; 15]);
    push(&mut timeline, 1, 5, 3, &[255; 15]);
    let snapshot = timeline.snapshot();
    assert_eq!(
        snapshot
            .episodes
            .iter()
            .map(|episode| episode.cell)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    let bounds = snapshot
        .episodes
        .iter()
        .map(|episode| episode.bounds)
        .collect::<Vec<_>>();
    assert_eq!(
        bounds,
        [
            Region {
                x: 0,
                y: 0,
                width: 2,
                height: 1
            },
            Region {
                x: 2,
                y: 0,
                width: 3,
                height: 1
            },
            Region {
                x: 0,
                y: 1,
                width: 2,
                height: 2
            },
            Region {
                x: 2,
                y: 1,
                width: 3,
                height: 2
            },
        ]
    );
}

#[test]
fn explicit_svg_rendering_uses_a_detached_snapshot_and_is_deterministic() {
    let mut timeline = timeline(2, 2, 1, 1);
    push(&mut timeline, 0, 2, 2, &[0; 4]);
    push(&mut timeline, 10, 2, 2, &[255; 4]);
    let snapshot = timeline.snapshot();
    let svg = snapshot.render_svg(SvgOptions::default()).unwrap();
    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.contains("<title>Episode 1; appeared 10 ms; duration 0 ms; 1 frames</title>"));
    assert_eq!(svg, snapshot.render_svg(SvgOptions::default()).unwrap());
    push(&mut timeline, 20, 2, 2, &[0; 4]);
    assert!(snapshot.episodes[0].is_active());
    assert!(!timeline.snapshot().episodes[0].is_active());
    assert_eq!(
        timeline.take_events().events.len(),
        2,
        "rendering does not consume watch events"
    );
}

#[test]
fn invalid_regions_grids_and_buffers_are_rejected_without_allocating_a_baseline() {
    for (region, columns, rows) in [
        (
            Region {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
            1,
            1,
        ),
        (
            Region {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            2,
            1,
        ),
        (
            Region {
                x: u32::MAX,
                y: 0,
                width: 1,
                height: 1,
            },
            1,
            1,
        ),
        (
            Region {
                x: 0,
                y: 0,
                width: 65,
                height: 65,
            },
            65,
            65,
        ),
    ] {
        assert!(VisualTimeline::new(TimelineOptions::new(region, columns, rows)).is_err());
    }
    assert!(Frame::gray(2, 2, &[0; 3]).is_err());
    assert!(Frame::new(1, 2, usize::MAX, PixelFormat::Gray8, &[]).is_err());
    let timeline = timeline(1, 1, 1, 1);
    assert_eq!(timeline.baseline_bytes(), 0);
    assert!(
        timeline
            .snapshot()
            .render_svg(SvgOptions::default())
            .is_err()
    );
}
