//! The strip drawn under a transparent title bar (D5c), as on macOS without a native menu bar,
//! which is how it can be tested on any system.

use crate::title_bar;

use super::*;

fn start_with_title_bar(dir: &Path) -> Harness<'static, DbRoverApp> {
    let (_layer, feed) = log_console::pipeline();
    let config = engine_config(dir);
    let mut harness = harness_at_default_size().build_eframe(|cc| {
        DbRoverApp::new(cc, feed, config)
            .unwrap()
            .with_integrated_title_bar()
    });
    crate::test_support::wait_for_startup(&mut harness);
    harness
}

fn menu_left(harness: &Harness<'_, DbRoverApp>) -> f32 {
    harness.get_by_label("File").rect().left()
}

#[test]
fn leaves_room_for_the_window_buttons_and_shows_the_title() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_title_bar(dir.path());

    let titles: Vec<_> = harness
        .query_all_by_label("DbRover")
        .map(|node| node.rect())
        .collect();
    let Some(title) = titles
        .iter()
        .find(|rect| rect.bottom() <= title_bar::HEIGHT)
    else {
        panic!("no title in the strip, only {titles:?}");
    };
    let centre = title.center().x;
    let width = crate::DEFAULT_WINDOW_SIZE[0];
    assert!(
        (centre - width / 2.0).abs() < 1.0,
        "the title is at {centre}"
    );
    assert!(menu_left(&harness) >= title_bar::WINDOW_BUTTONS_WIDTH);
    assert!(harness.get_by_label("File").rect().bottom() <= title_bar::HEIGHT);

    // The menu in the strip still works.
    harness.get_by_label("Help").click();
    harness.run_steps(2);
    harness.get_by_label("About DbRover").click();
    harness.run_steps(2);
    harness.get_by_label("Licensed under MIT OR Apache-2.0");
}

#[test]
fn full_screen_has_no_window_buttons_to_leave_room_for() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = start_with_title_bar(dir.path());

    if let Some(viewport) = harness
        .input_mut()
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
    {
        viewport.fullscreen = Some(true);
    }
    harness.run_steps(2);
    assert!(menu_left(&harness) < title_bar::WINDOW_BUTTONS_WIDTH);
}
