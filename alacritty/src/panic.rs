//! Panic reports on disk.

use std::backtrace::Backtrace;
use std::fs::{self, File};
use std::io::{self, Write};
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, process, thread};

#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, MessageBoxW,
};

#[cfg(windows)]
use alacritty_terminal::tty::windows::win32_string;

/// Install a panic handler that saves every panic to a file.
///
/// An app started from the Dock or a launcher has no STDERR that anybody
/// keeps, so without the file the reason for a crash is gone. On Windows the
/// panic is also rendered in a classical error dialog box.
pub fn attach_handler() {
    install(report_dir());
}

fn install(dir: Option<PathBuf>) {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        if let Some(dir) = &dir {
            match write_report(dir, panic_info) {
                Ok(path) => eprintln!("Panic report saved to {}", path.display()),
                Err(err) => eprintln!("Unable to save the panic report: {err}"),
            }
        }

        default_hook(panic_info);

        #[cfg(windows)]
        show_dialog(panic_info);
    }));
}

/// Write one report file and return its path.
fn write_report(dir: &Path, panic_info: &PanicHookInfo<'_>) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs());
    let path = dir.join(format!("panic-{secs}-{}.log", process::id()));

    // Two threads can panic in the same second, both reports must land.
    let mut file = File::options().create(true).append(true).open(&path)?;
    writeln!(file, "alacritty {}", env!("VERSION"))?;
    writeln!(file, "thread: {}", thread::current().name().unwrap_or("unnamed"))?;
    writeln!(file, "{panic_info}")?;
    writeln!(file, "\n{}", Backtrace::force_capture())?;

    Ok(path)
}

/// Directory of the report files, one that survives a reboot.
fn report_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let dir = home_dir()?.join("Library/Logs/Alacritty");

    #[cfg(windows)]
    let dir = PathBuf::from(env::var_os("LOCALAPPDATA")?).join("alacritty").join("crashes");

    #[cfg(not(any(target_os = "macos", windows)))]
    let dir = match env::var_os("XDG_STATE_HOME") {
        Some(state) if !state.is_empty() => PathBuf::from(state),
        _ => home_dir()?.join(".local/state"),
    }
    .join("alacritty/crashes");

    Some(dir)
}

#[cfg(not(windows))]
fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}

#[cfg(windows)]
fn show_dialog(panic_info: &PanicHookInfo<'_>) {
    let msg = format!("{}\n\nPress Ctrl-C to Copy", panic_info);
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            win32_string(&msg).as_ptr(),
            win32_string("Alacritty: Runtime Error").as_ptr(),
            MB_ICONERROR | MB_OK | MB_SETFOREGROUND | MB_TASKMODAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_is_saved_to_disk() {
        let dir = env::temp_dir().join(format!("alacritty-panic-test-{}", process::id()));

        install(Some(dir.clone()));
        let result = thread::spawn(|| panic!("the report must hold this text")).join();
        drop(panic::take_hook());
        assert!(result.is_err());

        // Another test can panic while the hook is installed and add its own file.
        let reports: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        fs::remove_dir_all(&dir).unwrap();

        let report = reports.iter().find(|text| text.contains("the report must hold this text"));
        let report = report.expect("no report holds the panic message");
        assert!(report.contains(concat!("alacritty ", env!("VERSION"))));
        assert!(report.contains("panic.rs"));
    }
}
