//! Cleanup of the title an app sets through an escape sequence.
//!
//! On Windows the title comes from the console host, not from the shell. A
//! shell that never sets one gets the full path of its exe, and an elevated
//! shell gets an `Administrator: ` prefix on top. macOS and Linux tabs show a
//! short process name, so the Windows title is cut down to match.

/// Prefix the console host puts in front of every title of an elevated shell.
#[cfg(any(windows, test))]
const ADMIN_PREFIX: &str = "Administrator: ";

/// Title to show for a tab, `None` when the app set an empty one.
pub fn normalize_terminal_title(title: String) -> Option<String> {
    #[cfg(windows)]
    let title = clean_console_title(&title).to_owned();

    (!title.is_empty()).then_some(title)
}

#[cfg(any(windows, test))]
fn clean_console_title(title: &str) -> &str {
    let title = title.strip_prefix(ADMIN_PREFIX).unwrap_or(title);
    exe_name(title).unwrap_or(title)
}

/// Bare name of a title that is the path of an exe, like `pwsh` for
/// `C:\Program Files\PowerShell\7\pwsh.exe`.
#[cfg(any(windows, test))]
fn exe_name(title: &str) -> Option<&str> {
    let file = title.rsplit_once(['\\', '/'])?.1;
    let (name, extension) = file.rsplit_once('.')?;
    (extension.eq_ignore_ascii_case("exe") && !name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::clean_console_title;

    #[test]
    fn exe_path_becomes_bare_name() {
        assert_eq!(clean_console_title(r"C:\Program Files\PowerShell\7\pwsh.exe"), "pwsh");
        assert_eq!(clean_console_title(r"C:\WINDOWS\system32\cmd.EXE"), "cmd");
    }

    #[test]
    fn admin_prefix_is_dropped() {
        assert_eq!(
            clean_console_title(r"Administrator: C:\Program Files\PowerShell\7\pwsh.exe"),
            "pwsh"
        );
        assert_eq!(clean_console_title("Administrator: lazygit"), "lazygit");
    }

    #[test]
    fn title_set_by_an_app_is_kept() {
        assert_eq!(clean_console_title("vim notes.exe"), "vim notes.exe");
        assert_eq!(clean_console_title(r"C:\dev\thing"), r"C:\dev\thing");
        assert_eq!(clean_console_title(""), "");
    }
}
