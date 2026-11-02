//! The menu bar the operating system draws, where DbRover has one: macOS. Elsewhere
//! [`NativeMenu`] has no values, so the app always draws egui's menu bar there, and the code
//! that uses it compiles everywhere without `cfg`s of its own.

use dbrover_export::CustomFormat;
use dbrover_store::CustomAction;

use crate::commands::AppCommand;
#[cfg(not(target_os = "macos"))]
use crate::commands::CommandState;
#[cfg(not(target_os = "macos"))]
use crate::keymap::Keymap;
#[cfg(target_os = "macos")]
pub use crate::macos::menu::NativeMenu;

/// What the user's own files add to the menus: custom actions, and formats to copy as.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar reads them")
)]
#[derive(Debug, Clone, Copy)]
pub struct CustomEntries<'a> {
    pub actions: &'a [CustomAction],
    pub formats: &'a [CustomFormat],
}

/// What picking a native menu item asks of the app.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS menu bar has it")
)]
#[derive(Debug, Clone, PartialEq)]
pub enum Chosen {
    Command(AppCommand),
    /// Input for the focused egui widget, as if keys had been pressed: the Edit menu.
    Input(Vec<egui::Event>),
}

#[cfg(not(target_os = "macos"))]
pub enum NativeMenu {}

#[cfg(not(target_os = "macos"))]
impl NativeMenu {
    pub fn drain(&mut self) -> Vec<Chosen> {
        match *self {}
    }

    pub fn sync(
        &mut self,
        _: &CommandState,
        _: &Keymap,
        _: &[std::path::PathBuf],
        _: CustomEntries<'_>,
    ) {
        match *self {}
    }
}
