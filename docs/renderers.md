# Renderers: drawing without OpenGL 2

Status: **adopted on 2026-10-31**, for 0.19.0. DbRover draws with glow (OpenGL) everywhere, and
on Windows falls back to wgpu on Direct3D 12 when glow can't open the window. Whatever stops
DbRover from starting is shown in a message box, on every platform.

## The problem

egui_glow needs OpenGL 2.0 or later. Windows without a graphics driver offers only its built-in
OpenGL 1.1 (the "GDI Generic" renderer): virtual machines without GPU passthrough, Windows
Sandbox, some remote desktop sessions, and CI validators such as winget's. There, eframe fails
with "egui_glow requires opengl 2.0+", `main` returned the error, and DbRover printed it to a
console nobody sees: started from the Start menu, it looked like nothing happened. winget's
validation of the first package ([winget-pkgs#464341]) reported that launching the installed
copy "returned an unexpected result"; a missing Visual C++ runtime is the likelier cause (fixed
separately), and a missing OpenGL driver the second suspect.

Every Windows 10 and 11, by contrast, has Direct3D 12 with WARP, Microsoft's software
rasteriser, which DXGI lists as the "Microsoft Basic Render Driver" adapter when there is no
GPU. wgpu's Direct3D 12 backend can draw with it.

[winget-pkgs#464341]: https://github.com/microsoft/winget-pkgs/pull/464341

## Options

| Option | Starts without OpenGL 2 | Cost |
| --- | --- | --- |
| (a) wgpu as the renderer | yes | wgpu on every machine: slower to start than glow, and a larger binary on every platform unless limited to Windows |
| (a') wgpu as a fallback after glow | yes | wgpu only where glow fails; the binary grows where wgpu is compiled in |
| (b) glow, plus a message box | no: the user learns why, but can't use DbRover | none |
| **(c) both: fallback and message box** | **yes, and anything else that stops DbRover is explained** | **(a')'s, on Windows only** |

**Chosen: (c), with wgpu compiled into Windows builds only, on the Direct3D 12 backend only.**

- **Why a fallback, not wgpu everywhere:** glow is what DbRover has always drawn with, it opens
  the window sooner (see [Costs](#costs)), and machines with a driver gain nothing from wgpu.
- **Why Windows only:** macOS always has OpenGL 2+ (deprecated, but present on every Mac
  DbRover supports), and Linux desktops get Mesa's software OpenGL (`llvmpipe`) without a GPU,
  so glow already draws there. The workspace's resolver keeps features of `cfg(windows)`
  dependencies out of the other targets: `cargo tree --target aarch64-apple-darwin -i wgpu` and
  the same for `x86_64-unknown-linux-gnu` print nothing.
- **Why Direct3D 12 only:** it is the backend that has a software adapter on every Windows. With
  only `dx12` enabled, wgpu leaves out Vulkan, OpenGL ES and their loaders. The fallback also
  sets the backend list to Direct3D 12, so test builds, which compile the others for
  egui_kittest, behave like releases. Shaders compile with FXC (`d3dcompiler_47.dll`, part of
  Windows) unless a DXC library is around.
- **Why the message box everywhere:** a DbRover started from the Dock or the application menu
  fails as silently as one started from the Start menu. It uses `rfd`, already a dependency for
  file dialogs, so it adds nothing.

## How it works

`renderer::run_native` (crates/dbrover-app/src/renderer.rs) tries each renderer in
`RENDERERS` (glow, then wgpu on Windows) until one opens the window.

- **Retrying on the same event loop.** winit allows one event loop per process. eframe's
  `run_and_return` (on by default) keeps it in a thread-local and reuses it, so a second
  `eframe::run_native` after a failed one works. Checked on macOS with glow forced to fail (an
  impossible stencil buffer): the second attempt drew with wgpu.
- **The app is created once.** `eframe::run_native` takes the app's creator by value and drops
  it on failure. DbRover's creator moves the engine's configuration and the log feed in, so the
  wrapper hands eframe a closure that borrows a `Cell` holding the creator, and takes it back
  after a failed attempt.
- **What counts as a renderer failure.** Only an error while the creator is still in the
  `Cell`: once eframe has called it, the window is open, and an error is DbRover's own, which
  another renderer wouldn't fix. Glow's failures without OpenGL 2 are errors, not panics, so
  they are retried.
- **When nothing works**, the error lists each renderer's reason and suggests updating the
  graphics driver. `startup_failure::report` logs it and shows it in a message box with the
  logs folder. The same happens to any other failure on the way to the window (no home
  directory, the logging set-up) and to a panic, which wgpu uses for some of its errors.
  A panic during the glow attempt is not retried: after one, eframe's event loop can't be
  trusted for another attempt.

## Costs

Measured on 2026-10-31. Baselines are v0.18.0's release run and assets; the wgpu numbers come
from this change's release dry run on `windows-2022` and its CI run on `windows-latest`.

**Size**, Windows only. macOS and Linux build exactly as before: their dry-run artifacts were
within 0.1 MB of v0.18.0's, which is the two commits merged since then.

| Artefact | v0.18.0 | With wgpu (Direct3D 12) | Change |
| --- | --- | --- | --- |
| `dbrover.exe` | 67.9 MB | 72.9 MB | +5.0 MB (+7.4%) |
| **Windows installer** | **27.4 MB** | **28.8 MB** | **+1.4 MB (+5.1%)** |

The Windows release build took 17m19s, against v0.18.0's 17m21s.

**Start-up**, time from `run_native` to the first frame:

| Where | glow | wgpu |
| --- | --- | --- |
| macOS, Apple silicon (release build, minimal app) | 0.57 s | 0.82 s (Metal, only to compare) |
| Windows CI runner, no GPU (debug build, minimal app) | fails: OpenGL 1.1 | 1.53 s on WARP, the process's first window |
| Same runner, `RENDERERS` in order | | 0.75 s, the failed glow attempt included |

On machines where glow works, nothing changes, because the wgpu code is never reached. The
0.75 s run came second in its process, so Direct3D's libraries were already loaded. A first
start on a GPU-less machine is closer to the 1.5 s run. The test doesn't time the failed glow
attempt on its own.

## Testing

- **CI, Windows** (`cargo test` in the existing `check` job): GitHub's Windows runners have no
  GPU, like the machines this is for. `renderer::tests` opens a real window twice: once with
  wgpu told to pick the WARP adapter, asserting it found one, and once with `RENDERERS` in
  order, as DbRover starts. Each run writes the renderer it drew with and its time to the first
  frame to the job's log (lines starting `WARP:` and `RENDERERS:`).
- **Unit test, everywhere:** the message box's text.
- **By hand, in Windows Sandbox**, which has no OpenGL 2 driver: see the pull request.
