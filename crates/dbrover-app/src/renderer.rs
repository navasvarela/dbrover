//! Opens DbRover's window with the first renderer that works on this computer.

use std::cell::Cell;

use anyhow::anyhow;
use eframe::{CreationContext, NativeOptions, Renderer};

/// What an app creator may fail with, as eframe declares it (eframe doesn't export its alias).
type DynError = Box<dyn std::error::Error + Send + Sync>;

/// The renderers tried in turn until one opens the window.
///
/// glow (OpenGL) comes first everywhere: DbRover has always drawn with it, and it starts
/// fastest. Windows computers without a graphics driver (virtual machines, Windows Sandbox, some
/// remote desktop sessions) offer only OpenGL 1.1, which egui_glow can't draw with, but every
/// Windows 10 and 11 has Direct3D 12 with WARP, its software renderer, so there wgpu follows.
/// Only Windows builds carry wgpu; docs/renderers.md has the trade-off and the costs.
pub const RENDERERS: &[Renderer] = &[
    Renderer::Glow,
    #[cfg(windows)]
    Renderer::Wgpu,
];

/// Like [`eframe::run_native`], but tries each of `renderers` until one opens the window.
///
/// A renderer only counts as failed while the app hasn't been created: once it has, the window
/// is open, and an error is the app's own, which another renderer wouldn't fix.
///
/// # Errors
/// Returns the app's error, or, when no renderer opened the window, each renderer's reason.
pub fn run_native(
    app_name: &str,
    options: NativeOptions,
    renderers: &[Renderer],
    create_app: impl FnOnce(&CreationContext<'_>) -> Result<Box<dyn eframe::App>, DynError>,
) -> anyhow::Result<()> {
    let mut options = options;
    let mut create_app = create_app;
    let mut failures = Vec::new();
    for &renderer in renderers {
        // eframe's clone leaves out the hooks, such as the one that builds the event loop. Only
        // the first attempt needs them: eframe keeps its event loop for the attempts after it.
        let next_options = options.clone();
        let attempt = options_for(std::mem::replace(&mut options, next_options), renderer);
        let unused = Cell::new(Some(create_app));
        let result = eframe::run_native(
            app_name,
            attempt,
            Box::new(|cc| {
                let create_app = unused.take().ok_or("eframe created the app twice")?;
                create_app(cc)
            }),
        );
        match (result, unused.into_inner()) {
            (Err(error), Some(not_created)) => {
                tracing::warn!("the {renderer} renderer could not open the window: {error}");
                failures.push(format!("{renderer}: {error}"));
                create_app = not_created;
            }
            // eframe's error wraps winit/GL errors that aren't `Sync`, so it can't become an
            // `anyhow::Error` directly.
            (result, _) => {
                return result.map_err(|error| anyhow!("DbRover stopped with an error: {error}"));
            }
        }
    }
    Err(anyhow!(
        "DbRover found no way to draw its window on this computer.{}\n\nUpdating the graphics \
         driver usually fixes this.",
        failures
            .iter()
            .map(|failure| format!("\n- {failure}"))
            .collect::<String>()
    ))
}

fn options_for(options: NativeOptions, renderer: Renderer) -> NativeOptions {
    #[cfg(windows)]
    let options = NativeOptions {
        wgpu_options: only_direct3d_12(options.wgpu_options),
        ..options
    };
    NativeOptions {
        renderer,
        ..options
    }
}

/// Vulkan and OpenGL need a graphics driver; Direct3D 12 falls back to WARP without one. Release
/// builds carry only the Direct3D 12 backend anyway, but tests build wgpu with the others too.
#[cfg(windows)]
fn only_direct3d_12(
    mut wgpu_options: eframe::egui_wgpu::WgpuConfiguration,
) -> eframe::egui_wgpu::WgpuConfiguration {
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
    }
    wgpu_options
}

// GitHub's Windows runners have no GPU, like the computers the fallback is for, so these tests
// open real windows there. macOS only lets the main thread open them, which a test isn't.
#[cfg(test)]
#[cfg(windows)]
mod tests {
    use std::cell::OnceCell;
    use std::io::Write as _;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    use winit::platform::windows::EventLoopBuilderExtWindows as _;

    use super::*;

    /// What the window was drawn with, and how long it took to show its first frame.
    #[derive(Clone)]
    struct Drawn {
        renderer: String,
        first_frame_after: Duration,
    }

    /// Closes its window as soon as it has drawn a frame, recording what drew it.
    struct CloseAfterFirstFrame {
        started: Instant,
        renderer: String,
        drawn: Rc<OnceCell<Drawn>>,
    }

    impl eframe::App for CloseAfterFirstFrame {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.drawn.get_or_init(|| Drawn {
                renderer: self.renderer.clone(),
                first_frame_after: self.started.elapsed(),
            });
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn describe(cc: &CreationContext<'_>) -> String {
        match &cc.wgpu_render_state {
            Some(state) => {
                let adapter = state.adapter.get_info();
                format!("wgpu on {} ({:?})", adapter.name, adapter.device_type)
            }
            None => "glow".to_owned(),
        }
    }

    fn open_and_close(options: NativeOptions, renderers: &[Renderer]) -> Drawn {
        let started = Instant::now();
        let drawn = Rc::new(OnceCell::new());
        let app_drawn = Rc::clone(&drawn);
        run_native("DbRover renderer test", options, renderers, move |cc| {
            Ok(Box::new(CloseAfterFirstFrame {
                started,
                renderer: describe(cc),
                drawn: app_drawn,
            }))
        })
        .unwrap();
        drawn
            .get()
            .cloned()
            .expect("the window closed before drawing a frame")
    }

    /// Writes past the test harness's capture, so CI's log shows what each run cost.
    fn report(run: &str, drawn: &Drawn) {
        writeln!(
            std::io::stderr(),
            "{run}: drawn with {} in {:?}",
            drawn.renderer,
            drawn.first_frame_after
        )
        .unwrap();
    }

    fn pick_warp(
        adapters: &[wgpu::Adapter],
        _surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<wgpu::Adapter, String> {
        adapters
            .iter()
            .find(|adapter| adapter.get_info().device_type == wgpu::DeviceType::Cpu)
            .cloned()
            .ok_or_else(|| "Direct3D 12 offers no WARP adapter".to_owned())
    }

    // One test, because winit allows one event loop per process and eframe keeps it per thread.
    #[test]
    fn warp_draws_the_window_and_the_renderers_in_order_open_it() {
        let state = tempfile::tempdir().unwrap();
        let persistence_path = Some(state.path().join("window.ron"));
        let mut warp_setup = WgpuSetupCreateNew::without_display_handle();
        warp_setup.native_adapter_selector = Some(Arc::new(pick_warp));
        let warp = NativeOptions {
            event_loop_builder: Some(Box::new(|builder| {
                builder.with_any_thread(true);
            })),
            persistence_path: persistence_path.clone(),
            wgpu_options: WgpuConfiguration {
                wgpu_setup: WgpuSetup::CreateNew(warp_setup),
                ..Default::default()
            },
            ..Default::default()
        };
        let in_order = NativeOptions {
            persistence_path,
            ..Default::default()
        };

        let drawn = open_and_close(warp, &[Renderer::Wgpu]);
        report("WARP", &drawn);
        assert!(drawn.renderer.contains("(Cpu)"), "{}", drawn.renderer);

        report("RENDERERS", &open_and_close(in_order, RENDERERS));
    }
}
