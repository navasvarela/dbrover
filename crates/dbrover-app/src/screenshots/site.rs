//! Renders the website's pictures and records its clips, headlessly, like the manual's
//! screenshots. Ignored by default, because it needs a GPU adapter and writes many files:
//!
//! ```text
//! cargo test -p dbrover-app record_site_media -- --ignored
//! ```
//!
//! Everything goes to `target/site-media`, or `DBROVER_SITE_MEDIA_DIR` when set, in both themes:
//! each still as `<scene>-<theme>.png`, and each clip as numbered frames in `<clip>-<theme>/`
//! with a `frames.ffconcat` saying how long each frame shows. `site/clips.sh` encodes them into
//! `site/media/`. `DBROVER_SITE_MEDIA=hero,grid-changes` renders only the clips and stills named.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Color32, Event, Key, Modifiers, PointerButton, Pos2, Rect, Theme, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use image::{Rgba, RgbaImage};

use super::{
    Fixture, FixtureSources, PIXELS_PER_POINT, Region, SCENES, has_servers_for, render_scene,
    theme_name,
};
use crate::app::DbRoverApp;
use crate::test_support::step_until_visible;

/// Smaller than the manual's window, so the interface stays legible at the width the page
/// shows a picture.
const SITE_WINDOW_SIZE: Vec2 = Vec2::new(1024.0, 720.0);

/// Wider than `SITE_WINDOW_SIZE`, because a query plan's columns, **Time** last, don't fit
/// beside the explorer in a window 1024 points wide.
const PLAN_WINDOW_SIZE: Vec2 = Vec2::new(1280.0, 720.0);

/// A scene of the manual's that the feature tour shows, the window it plays in, and the part of
/// the window to show.
struct TourStill {
    scene: &'static str,
    window: Vec2,
    region: Region,
}

/// One per row of the tour, cropped to what the row is about, so it stays legible in a column
/// half the page wide.
const TOUR_STILLS: &[TourStill] = &[
    TourStill {
        scene: "problems",
        window: SITE_WINDOW_SIZE,
        region: console_editor,
    },
    TourStill {
        scene: "grid-changes",
        window: SITE_WINDOW_SIZE,
        region: console,
    },
    TourStill {
        scene: "modify-table",
        window: SITE_WINDOW_SIZE,
        region: dialog,
    },
    TourStill {
        scene: "explain-plan",
        window: PLAN_WINDOW_SIZE,
        region: plan,
    },
    TourStill {
        scene: "careful-confirmation",
        window: SITE_WINDOW_SIZE,
        region: console_and_dialog,
    },
    TourStill {
        scene: "project-settings",
        window: SITE_WINDOW_SIZE,
        region: dialog,
    },
    TourStill {
        scene: "ssh-data-source-dialog",
        window: SITE_WINDOW_SIZE,
        region: dialog,
    },
];

/// Room around a cropped region of the console, which the console's own background fills.
const CROP_MARGIN: f32 = 16.0;
/// Room around a dialog for its rounded corners: any more shows the app behind it.
const DIALOG_MARGIN: f32 = 4.0;

/// The largest window or modal open over the app. Both are egui areas above
/// the background layer; a modal's backdrop is one too, but it covers the whole app.
fn dialog(harness: &Harness<'static, DbRoverApp>) -> Option<Rect> {
    let is_inside_app = |rect: &Rect| rect.width() < SITE_WINDOW_SIZE.x - 1.0;
    harness.ctx.memory(|memory| {
        memory
            .areas()
            .visible_layer_ids()
            .into_iter()
            .filter(|layer| layer.order != egui::Order::Background)
            .filter_map(|layer| memory.area_rect(layer.id))
            .filter(is_inside_app)
            .max_by(|a, b| a.area().total_cmp(&b.area()))
            .map(|rect| rect.expand(DIALOG_MARGIN))
    })
}

/// How far above the log console's search field its panel starts, tab bar included: measured
/// on `SITE_WINDOW_SIZE`. egui_dock's tab titles aren't in the accessibility tree, so the
/// panel's edge is found from the field.
const LOG_CONSOLE_HEADER: f32 = 48.0;

/// The focused console, from its toolbar down to the log console.
fn console(harness: &Harness<'static, DbRoverApp>) -> Option<Rect> {
    let toolbar = harness.query_by_label("Run")?.rect();
    let editor = harness.query_by_role(Role::MultilineTextInput)?.rect();
    let log_search = harness
        .query_by(|node| node.placeholder() == Some("Search"))?
        .rect();
    Some(Rect::from_min_max(
        Pos2::new(editor.min.x - CROP_MARGIN, toolbar.min.y - CROP_MARGIN),
        Pos2::new(
            harness.ctx.content_rect().max.x,
            log_search.min.y - LOG_CONSOLE_HEADER,
        ),
    ))
}

/// The console down to just under its editor, with room for a tooltip below it.
fn console_editor(harness: &Harness<'static, DbRoverApp>) -> Option<Rect> {
    const TOOLTIP_ROOM: f32 = 96.0;
    let area = console(harness)?;
    let editor = harness.query_by_role(Role::MultilineTextInput)?.rect();
    Some(Rect::from_min_max(
        area.min,
        Pos2::new(area.max.x, (editor.max.y + TOOLTIP_ROOM).min(area.max.y)),
    ))
}

/// The console's query plan, from its **Tree** and **Text** toggles down, without the editor
/// above it.
fn plan(harness: &Harness<'static, DbRoverApp>) -> Option<Rect> {
    let area = console(harness)?;
    let toggles = harness.query_by_label("Tree")?.rect();
    Some(Rect::from_min_max(
        Pos2::new(area.min.x, toggles.min.y - CROP_MARGIN),
        area.max,
    ))
}

/// The console's editor and the dialog its statement opened.
fn console_and_dialog(harness: &Harness<'static, DbRoverApp>) -> Option<Rect> {
    Some(console_editor(harness)?.union(dialog(harness)?))
}

const BOTH_THEMES: [Theme; 2] = [Theme::Light, Theme::Dark];

struct Clip {
    name: &'static str,
    data_sources: FixtureSources,
    play: fn(&mut Recorder),
}

const CLIPS: &[Clip] = &[Clip {
    name: "hero",
    data_sources: FixtureSources::Shop,
    play: hero,
}];

#[test]
#[ignore = "needs a GPU adapter and writes the website's media; run with --ignored"]
fn record_site_media() {
    let out_dir = site_media_dir();
    std::fs::create_dir_all(&out_dir).unwrap();
    let wanted = wanted_media();
    let is_wanted = |name: &str| {
        wanted
            .as_ref()
            .is_none_or(|names| names.iter().any(|wanted| wanted == name))
    };

    for still in TOUR_STILLS.iter().filter(|still| is_wanted(still.scene)) {
        let scene = SCENES
            .iter()
            .find(|scene| scene.name == still.scene)
            .unwrap_or_else(|| panic!("no scene {:?}", still.scene));
        if !has_servers_for(scene) {
            continue;
        }
        for theme in BOTH_THEMES {
            let path = out_dir.join(format!("{}-{}.png", scene.name, theme_name(theme)));
            render_scene(scene, theme, still.window, still.region)
                .save(&path)
                .unwrap();
            println!("wrote {}", path.display());
        }
    }

    for clip in CLIPS.iter().filter(|clip| is_wanted(clip.name)) {
        for theme in BOTH_THEMES {
            let frames_dir = out_dir.join(format!("{}-{}", clip.name, theme_name(theme)));
            let fixture = Fixture::new(clip.data_sources);
            let mut recorder = Recorder::new(fixture.start(theme, SITE_WINDOW_SIZE), &frames_dir);
            (clip.play)(&mut recorder);
            let length = recorder.finish();
            println!("wrote {} ({length:.1?})", frames_dir.display());
        }
    }
}

fn site_media_dir() -> PathBuf {
    std::env::var_os("DBROVER_SITE_MEDIA_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/site-media"),
        PathBuf::from,
    )
}

/// The names `DBROVER_SITE_MEDIA` lists, or `None` for everything.
fn wanted_media() -> Option<Vec<String>> {
    let names = std::env::var("DBROVER_SITE_MEDIA").ok()?;
    Some(
        names
            .split(',')
            .map(|name| name.trim().to_owned())
            .collect(),
    )
}

/// Opens `shop`, expands its tables, types a query with completion's help, runs it, and has
/// the database filter and sort the results.
fn hero(recorder: &mut Recorder) {
    recorder.pause(Duration::from_millis(700));
    for node in ["shop", "main", "tables"] {
        recorder.click(&format!("Expand {node}"));
    }
    recorder.wait_until_visible("Expand orders");
    recorder.pause(Duration::from_millis(500));

    recorder.click_secondary("shop");
    recorder.click_first("New query console");
    recorder.wait_for_editor();
    recorder.type_text("SELECT * FROM ord");
    recorder.wait_until_visible("order_totals");
    recorder.pause(Duration::from_millis(500));
    recorder.type_text("ers;");
    recorder.pause(Duration::from_millis(300));

    recorder.click("Run");
    recorder.wait_until_visible("60 rows");
    recorder.pause(Duration::from_millis(900));

    recorder.click("In the database");
    recorder.click_placeholder("Filter every row, then Enter");
    recorder.type_text("open");
    recorder.press(Key::Enter);
    recorder.wait_until_visible("Sorted and filtered by the database");
    recorder.pause(Duration::from_millis(900));
    // Twice, for newest first: the last open order is the 60th, placed on 30 June.
    recorder.click("placed_at");
    recorder.click("placed_at");
    recorder.wait_until_visible("2026-06-30");
    recorder.pause(Duration::from_millis(2000));
}

/// One frame of the clip's output; holds are rounded to whole frames.
const FRAME: Duration = Duration::from_micros(33_333);
/// How long a typed character shows before the next: brisk, but readable.
const KEYSTROKE: Duration = Duration::from_millis(65);
/// Frames the pointer takes from one target to the next.
const POINTER_TRAVEL_FRAMES: u32 = 12;
/// Frames the ring around a click lasts, growing and fading.
const CLICK_RING_FRAMES: u32 = 9;

/// Drives an app the way a user would, rendering a frame after each step of the script, with
/// a pointer drawn in (egui draws none) so a viewer can follow the clicks.
///
/// Time in the clip is the script's, not the clock's: waiting for the engine adds no frames,
/// so a slow machine records the same clip as a fast one.
struct Recorder {
    harness: Harness<'static, DbRoverApp>,
    dir: PathBuf,
    /// Each frame's file name and how long it shows.
    frames: Vec<(String, Duration)>,
    pointer: Pos2,
    frames_since_click: Option<u32>,
}

impl Recorder {
    fn new(harness: Harness<'static, DbRoverApp>, dir: &Path) -> Self {
        if dir.exists() {
            std::fs::remove_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(dir).unwrap();
        let pointer = (SITE_WINDOW_SIZE / 2.0).to_pos2();
        Self {
            harness,
            dir: dir.to_owned(),
            frames: Vec::new(),
            pointer,
            frames_since_click: None,
        }
    }

    /// Steps the app once and renders a frame that shows for `hold`.
    fn frame(&mut self, hold: Duration) {
        self.harness.step();
        let mut image = self.harness.render().unwrap();
        if let Some(frames) = self.frames_since_click {
            draw_click_ring(&mut image, self.pointer, frames);
            self.frames_since_click = Some(frames + 1).filter(|&next| next < CLICK_RING_FRAMES);
        }
        draw_pointer(&mut image, self.pointer);
        let name = format!("frame-{:05}.png", self.frames.len());
        image.save(self.dir.join(&name)).unwrap();
        self.frames.push((name, hold.max(FRAME)));
    }

    /// Keeps the last frame on screen for `duration` longer, after letting a click's ring
    /// fade, which would otherwise stay on the held frame.
    fn pause(&mut self, mut duration: Duration) {
        while self.frames.is_empty() || self.frames_since_click.is_some() {
            self.frame(FRAME);
            duration = duration.saturating_sub(FRAME);
        }
        if let Some((_, hold)) = self.frames.last_mut() {
            *hold += duration;
        }
    }

    /// Moves the pointer to `target` over a few frames, easing in and out.
    fn move_to(&mut self, target: Pos2) {
        let start = self.pointer;
        for frame in 1..=POINTER_TRAVEL_FRAMES {
            let t = frame as f32 / POINTER_TRAVEL_FRAMES as f32;
            let eased = t * t * (3.0 - 2.0 * t);
            self.pointer = start.lerp(target, eased);
            self.harness.event(Event::PointerMoved(self.pointer));
            self.frame(FRAME);
        }
    }

    fn click(&mut self, label: &str) {
        let target = self.target(label);
        self.click_at(target, PointerButton::Primary);
    }

    fn click_secondary(&mut self, label: &str) {
        let target = self.target(label);
        self.click_at(target, PointerButton::Secondary);
    }

    /// Clicks the first of several widgets labelled `label`: a menu's item, say, where a
    /// Welcome card has the same name.
    fn click_first(&mut self, label: &str) {
        step_until_visible(&mut self.harness, label);
        let target = self
            .harness
            .get_all_by_label(label)
            .next()
            .unwrap()
            .rect()
            .center();
        self.click_at(target, PointerButton::Primary);
    }

    /// Clicks the text field showing `placeholder`, once it shows it.
    fn click_placeholder(&mut self, placeholder: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let target = loop {
            if let Some(field) = self
                .harness
                .query_by(|node| node.placeholder() == Some(placeholder))
            {
                break field.rect().center();
            }
            assert!(Instant::now() < deadline, "{placeholder:?} never appeared");
            self.harness.step();
        };
        self.click_at(target, PointerButton::Primary);
    }

    /// The centre of the widget labelled `label`, once it is on screen.
    fn target(&mut self, label: &str) -> Pos2 {
        step_until_visible(&mut self.harness, label);
        self.harness.get_by_label(label).rect().center()
    }

    fn click_at(&mut self, target: Pos2, button: PointerButton) {
        self.move_to(target);
        for pressed in [true, false] {
            self.harness.event(Event::PointerButton {
                pos: target,
                button,
                pressed,
                modifiers: Modifiers::NONE,
            });
            if !pressed {
                self.frames_since_click = Some(0);
            }
            self.frame(FRAME * 2);
        }
    }

    /// Types `text` a character at a time into the focused widget.
    fn type_text(&mut self, text: &str) {
        for character in text.chars() {
            match character {
                '\n' => self.harness.key_press(Key::Enter),
                _ => self.harness.event(Event::Text(character.to_string())),
            }
            self.frame(KEYSTROKE);
        }
    }

    fn press(&mut self, key: Key) {
        self.harness.key_press(key);
        self.frame(KEYSTROKE * 2);
    }

    /// Steps until `text` appears, then shows it: however long that takes adds one frame.
    fn wait_until_visible(&mut self, text: &str) {
        step_until_visible(&mut self.harness, text);
        self.frame(FRAME);
    }

    /// Waits for a new console's editor to take the keyboard focus.
    fn wait_for_editor(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self
            .harness
            .query_by(|node| node.role() == Role::MultilineTextInput && node.is_focused())
            .is_none()
        {
            assert!(Instant::now() < deadline, "the editor never took the focus");
            if let Some(editor) = self.harness.query_by_role(Role::MultilineTextInput) {
                editor.focus();
            }
            self.harness.step();
        }
        self.frame(FRAME);
    }

    /// Writes `frames.ffconcat`, the timeline `site/clips.sh` encodes, and returns the clip's
    /// length.
    fn finish(self) -> Duration {
        let mut timeline = String::from("ffconcat version 1.0\n");
        for (name, hold) in &self.frames {
            writeln!(timeline, "file {name}\nduration {:.4}", hold.as_secs_f64()).unwrap();
        }
        // The concat demuxer ignores the last entry's duration unless the file comes again.
        if let Some((name, _)) = self.frames.last() {
            writeln!(timeline, "file {name}").unwrap();
        }
        std::fs::write(self.dir.join("frames.ffconcat"), timeline).unwrap();
        self.frames.iter().map(|(_, hold)| *hold).sum()
    }
}

/// The classic arrow pointer, in points from its tip, as macOS and most themes draw it.
const POINTER_SHAPE: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 16.0),
    (4.0, 12.5),
    (7.0, 19.0),
    (9.5, 18.0),
    (6.5, 11.5),
    (11.5, 11.5),
];
const POINTER_OUTLINE: f32 = 1.2;

/// Draws a black-edged white arrow with its tip at `tip`, in points.
fn draw_pointer(image: &mut RgbaImage, tip: Pos2) {
    let scale = PIXELS_PER_POINT;
    let shape = POINTER_SHAPE.map(|(x, y)| Pos2::new(tip.x + x, tip.y + y) * scale);
    let (min, max) = shape.iter().fold(
        (Pos2::new(f32::MAX, f32::MAX), Pos2::new(f32::MIN, f32::MIN)),
        |(min, max), point| (min.min(*point), max.max(*point)),
    );
    for y in (min.y.floor() as u32)..=(max.y.ceil() as u32) {
        for x in (min.x.floor() as u32)..=(max.x.ceil() as u32) {
            let pixel = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
            if x >= image.width() || y >= image.height() || !inside(&shape, pixel) {
                continue;
            }
            let colour = if distance_to_edge(&shape, pixel) < POINTER_OUTLINE * scale {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            image.put_pixel(x, y, Rgba(colour.to_array()));
        }
    }
}

/// Even-odd rule: a point is inside when a ray from it crosses the outline an odd number of
/// times.
fn inside(polygon: &[Pos2], point: Pos2) -> bool {
    let edges = polygon.iter().zip(polygon.iter().cycle().skip(1));
    edges
        .filter(|(a, b)| (a.y > point.y) != (b.y > point.y))
        .filter(|(a, b)| point.x < a.x + (point.y - a.y) / (b.y - a.y) * (b.x - a.x))
        .count()
        % 2
        == 1
}

fn distance_to_edge(polygon: &[Pos2], point: Pos2) -> f32 {
    let edges = polygon.iter().zip(polygon.iter().cycle().skip(1));
    edges
        .map(|(a, b)| {
            let along = *b - *a;
            let t = ((point - *a).dot(along) / along.length_sq()).clamp(0.0, 1.0);
            point.distance(*a + along * t)
        })
        .fold(f32::MAX, f32::min)
}

/// The brand's teal, which reads on both themes.
const CLICK_RING_COLOUR: [u8; 3] = [45, 212, 191];

/// A ring around the click that grows and fades over `CLICK_RING_FRAMES`.
fn draw_click_ring(image: &mut RgbaImage, centre: Pos2, frame: u32) {
    let progress = frame as f32 / CLICK_RING_FRAMES as f32;
    let radius = (6.0 + 14.0 * progress) * PIXELS_PER_POINT;
    let width = 2.0 * PIXELS_PER_POINT;
    let opacity = 0.85 * (1.0 - progress);
    let centre = centre * PIXELS_PER_POINT;
    let reach = radius + width;
    let left = (centre.x - reach).max(0.0) as u32;
    let top = (centre.y - reach).max(0.0) as u32;
    let right = ((centre.x + reach) as u32).min(image.width().saturating_sub(1));
    let bottom = ((centre.y + reach) as u32).min(image.height().saturating_sub(1));
    for y in top..=bottom {
        for x in left..=right {
            let distance = Pos2::new(x as f32 + 0.5, y as f32 + 0.5).distance(centre);
            let coverage = (1.0 - ((distance - radius).abs() - width / 2.0)).clamp(0.0, 1.0);
            if coverage > 0.0 {
                blend(
                    image.get_pixel_mut(x, y),
                    CLICK_RING_COLOUR,
                    opacity * coverage,
                );
            }
        }
    }
}

fn blend(pixel: &mut Rgba<u8>, colour: [u8; 3], alpha: f32) {
    for (channel, value) in pixel.0.iter_mut().zip(colour) {
        *channel = (f32::from(*channel) * (1.0 - alpha) + f32::from(value) * alpha).round() as u8;
    }
}
