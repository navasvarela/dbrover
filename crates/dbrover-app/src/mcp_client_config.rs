//! Copy MCP configuration: the entry an MCP client's configuration needs to start
//! `dbrover mcp` for a project, with this machine's path to DbRover.

use std::io;
use std::path::{Path, PathBuf};

use dbrover_store::{CONFIG_DIR_FLAG, ProjectSlug};
use serde_json::json;

/// Set by the AppImage runtime to the `.AppImage` file itself. The running executable is
/// inside the image's temporary mount, which is gone once DbRover quits, so a client
/// configured with that path couldn't start it.
const APPIMAGE: &str = "APPIMAGE";

/// The path a client should run: the executable inside the `.app` bundle on macOS (what
/// `current_exe` already is there), the AppImage on Linux, and the installed `.exe` on
/// Windows. Symbolic links are resolved, so the path keeps working whatever the link was.
pub fn executable() -> io::Result<PathBuf> {
    if let Some(appimage) = std::env::var_os(APPIMAGE).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(appimage));
    }
    let current = std::env::current_exe()?;
    Ok(current.canonicalize().unwrap_or(current))
}

/// Puts `slug`'s entry on the clipboard, with this machine's path to DbRover.
pub fn copy(ctx: &egui::Context, slug: &ProjectSlug, config_dir: Option<&Path>) {
    match executable() {
        Ok(executable) => {
            ctx.copy_text(client_configuration(slug, &executable, config_dir));
            tracing::info!(project = %slug, "copied the MCP configuration");
        }
        Err(error) => tracing::error!(
            "could not find DbRover's executable for the MCP configuration: {}",
            dbrover_core::describe_error(&error)
        ),
    }
}

/// The `mcpServers` entry for `slug`, as the JSON clients such as Claude Desktop, Claude Code
/// and Cursor read, named `dbrover-<slug>` so several projects can sit side by side. With
/// `config_dir`, the server is told the settings folder, which it would otherwise find as the
/// app does, without the app's flag or environment.
pub fn client_configuration(
    slug: &ProjectSlug,
    executable: &Path,
    config_dir: Option<&Path>,
) -> String {
    let mut args = vec!["mcp".to_owned(), "--project".to_owned(), slug.to_string()];
    if let Some(folder) = config_dir {
        args.extend([CONFIG_DIR_FLAG.to_owned(), folder.display().to_string()]);
    }
    let configuration = json!({
        "mcpServers": {
            format!("dbrover-{slug}"): {
                "command": executable.display().to_string(),
                "args": args,
            }
        }
    });
    // Serializing a `Value` built from strings can't fail; the fallback is never used.
    serde_json::to_string_pretty(&configuration).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_project_and_the_executable() {
        let slug = ProjectSlug::from_name("Analytics");
        let text = client_configuration(
            &slug,
            Path::new("/Applications/DbRover.app/Contents/MacOS/dbrover"),
            None,
        );

        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed,
            json!({
                "mcpServers": {
                    "dbrover-analytics": {
                        "command": "/Applications/DbRover.app/Contents/MacOS/dbrover",
                        "args": ["mcp", "--project", "analytics"]
                    }
                }
            })
        );
    }

    #[test]
    fn names_a_settings_folder_the_client_would_not_find() {
        let slug = ProjectSlug::from_name("Analytics");
        let text = client_configuration(
            &slug,
            Path::new("/usr/bin/dbrover"),
            Some(Path::new("/home/ada/work/dbrover")),
        );

        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed["mcpServers"]["dbrover-analytics"]["args"],
            json!([
                "mcp",
                "--project",
                "analytics",
                "--config-dir",
                "/home/ada/work/dbrover"
            ])
        );
    }

    #[test]
    fn finds_this_executable() {
        let path = executable().unwrap();
        assert!(path.is_absolute(), "{}", path.display());
    }
}
