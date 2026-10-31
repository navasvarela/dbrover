//! Tells the user why DbRover could not start, when it has no window to say it in.

use std::path::Path;

/// Logs why DbRover could not start and shows it in a message box.
///
/// DbRover is usually started from the Start menu, the Dock or a shortcut, where nobody reads
/// what it prints, so without the message box a failed start looks like nothing happening at all.
pub fn report(error: &anyhow::Error, logs_dir: Option<&Path>) {
    tracing::error!("DbRover could not start: {error:#}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("DbRover could not start")
        .set_description(message(error, logs_dir))
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

fn message(error: &anyhow::Error, logs_dir: Option<&Path>) -> String {
    match logs_dir {
        Some(logs_dir) => format!(
            "{error:#}\n\nThe log in {} has the details.",
            logs_dir.display()
        ),
        None => format!("{error:#}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::anyhow;

    use super::*;

    #[test]
    fn the_message_gives_the_reason_and_where_the_log_is() {
        let error = anyhow!("egui_glow requires opengl 2.0+.").context("no renderer worked");
        let logs_dir = PathBuf::from("C:/Users/ana/AppData/Roaming/DbRover/logs");

        assert_eq!(
            message(&error, Some(&logs_dir)),
            "no renderer worked: egui_glow requires opengl 2.0+.\n\n\
             The log in C:/Users/ana/AppData/Roaming/DbRover/logs has the details."
        );
        assert_eq!(
            message(&error, None),
            "no renderer worked: egui_glow requires opengl 2.0+."
        );
    }
}
