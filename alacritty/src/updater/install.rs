//! The install half of the self update. Downloads with `curl`, verifies
//! against `SHA256SUMS`, unpacks with `tar` and swaps the installed files. Both
//! tools ship with macOS, Linux and Windows 10 1803 and newer, the same ones
//! thing uses to install the fork in the first place.

use std::error::Error;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{fs, thread};

use log::{info, warn};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use super::{UpdateRelease, UpdateState};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// How often the growing download is measured for the percent.
const POLL: Duration = Duration::from_millis(200);
/// Seconds curl may spend on the asset, a slow link still fits.
const DOWNLOAD_TIMEOUT: &str = "600";
/// Seconds for the small requests, the release JSON and the sums.
const REQUEST_TIMEOUT: &str = "30";

#[cfg(windows)]
const SWAPPED_FILES: [&str; 3] = ["alacritty.exe", "conpty.dll", "OpenConsole.exe"];

pub fn run(release: &UpdateRelease, exe: &Path, report: &dyn Fn(UpdateState)) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("alacritty-update-{}", release.tag));
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    let result = install_from(release, exe, &dir, report);
    if let Err(err) = fs::remove_dir_all(&dir) {
        warn!("Could not remove {}: {err}", dir.display());
    }
    result
}

fn install_from(
    release: &UpdateRelease,
    exe: &Path,
    dir: &Path,
    report: &dyn Fn(UpdateState),
) -> Result<()> {
    let archive = dir.join(&release.asset_name);
    report(UpdateState::Downloading(0));
    download(&release.asset_url, &archive, release.size, report)?;
    verify(&archive, release)?;
    report(UpdateState::Unpacking);
    unpack(&archive, dir)?;
    report(UpdateState::Installing);
    swap(dir, exe)
}

/// The body of a small response, the sums file.
pub fn curl_text(url: &str) -> Result<String> {
    let output = curl(REQUEST_TIMEOUT).arg("-L").arg(url).stderr(Stdio::piped()).output()?;
    if !output.status.success() {
        return Err(
            format!("curl {url}: {}", String::from_utf8_lossy(&output.stderr).trim()).into()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// The response headers of a HEAD request, every hop when redirects are
/// followed, so a `location` can be read without following it.
pub fn curl_headers(url: &str, follow: bool) -> Result<String> {
    let mut command = curl(REQUEST_TIMEOUT);
    command.arg("-I");
    if follow {
        command.arg("-L");
    }
    let output = command.arg(url).stderr(Stdio::piped()).output()?;
    if !output.status.success() {
        return Err(
            format!("curl {url}: {}", String::from_utf8_lossy(&output.stderr).trim()).into()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Fetch the asset to `path`, reporting whole percents as the file grows.
fn download(url: &str, path: &Path, size: u64, report: &dyn Fn(UpdateState)) -> Result<()> {
    let mut child = curl(DOWNLOAD_TIMEOUT)
        .arg("-L")
        .arg("-o")
        .arg(path)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut last = 0;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        let done = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        let percent = (done * 100).checked_div(size).unwrap_or(0).min(100) as u8;
        if percent != last {
            last = percent;
            report(UpdateState::Downloading(percent));
        }
        thread::sleep(POLL);
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(
            format!("curl {url}: {}", String::from_utf8_lossy(&output.stderr).trim()).into()
        );
    }
    report(UpdateState::Downloading(100));
    Ok(())
}

fn verify(archive: &Path, release: &UpdateRelease) -> Result<()> {
    let sums = curl_text(&release.sums_url)?;
    let name = &release.asset_name;
    let expected =
        expected_sum(&sums, name).ok_or_else(|| format!("{name} is missing from SHA256SUMS"))?;
    let bytes = fs::read(archive)?;
    if bytes.len() as u64 != release.size {
        return Err(
            format!("{name} is {} bytes, the release says {}", bytes.len(), release.size).into()
        );
    }
    let digest: String = Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect();
    if digest != expected {
        return Err(format!("{name} sha256 is {digest}, SHA256SUMS says {expected}").into());
    }
    Ok(())
}

/// The hash on the `sha256sum` line for `name`, written as `<hex>  <name>`.
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(' ')?;
        (file.trim_start_matches([' ', '*']) == name).then(|| hash.to_lowercase())
    })
}

fn unpack(archive: &Path, dir: &Path) -> Result<()> {
    let output = Command::new(tool("tar"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags_no_window()
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "tar {}: {}",
            archive.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

fn curl(timeout: &str) -> Command {
    let mut command = Command::new(tool("curl"));
    command
        .args(["-fsS", "--user-agent", "alacritty", "--max-time", timeout])
        .stdin(Stdio::null())
        .creation_flags_no_window();
    command
}

/// The System32 copy on Windows, so the PowerShell `curl` alias and any tool
/// of the same name on PATH never get in the way.
#[cfg(windows)]
fn tool(name: &str) -> PathBuf {
    let system = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join(format!("{name}.exe")));
    match system {
        Some(path) if path.exists() => path,
        _ => PathBuf::from(name),
    }
}

#[cfg(not(windows))]
fn tool(name: &str) -> PathBuf {
    PathBuf::from(name)
}

/// A windows subsystem app opens a console window for every child unless the
/// child is created without one.
trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

impl NoWindow for Command {
    #[cfg(windows)]
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn old_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    name.push(".old");
    path.with_file_name(name)
}

/// A running alacritty locks its exe, its `conpty.dll` and `OpenConsole.exe`,
/// so a copy over them fails. Windows still allows renaming a locked file, so
/// each one is moved aside as `.old` and the new one copied in. The open
/// window keeps the old files until it is closed.
#[cfg(windows)]
fn swap(dir: &Path, exe: &Path) -> Result<()> {
    let bin_dir = exe.parent().ok_or("the install path has no directory")?;
    for name in SWAPPED_FILES {
        let new = dir.join(name);
        if !new.exists() {
            return Err(format!("{name} is missing from the release zip").into());
        }
        replace_locked(&new, &bin_dir.join(name))?;
    }
    Ok(())
}

#[cfg(windows)]
fn replace_locked(new: &Path, dest: &Path) -> Result<()> {
    let old = old_path(dest);
    clear_old(&old)?;
    if dest.exists() {
        fs::rename(dest, &old)?;
    }
    if let Err(err) = fs::copy(new, dest) {
        // Put the old file back so the install keeps working.
        if old.exists() {
            if let Err(restore) = fs::rename(&old, dest) {
                warn!("Could not restore {}: {restore}", dest.display());
            }
        }
        return Err(format!("copy to {}: {err}", dest.display()).into());
    }
    Ok(())
}

/// A rename over the running binary is safe on Linux, the process keeps the
/// old inode. The staged copy sits next to it so the rename stays on one
/// filesystem.
#[cfg(not(any(windows, target_os = "macos")))]
fn swap(dir: &Path, exe: &Path) -> Result<()> {
    let new = dir.join("alacritty").join("alacritty");
    if !new.exists() {
        return Err("alacritty is missing from the release tarball".into());
    }
    let staged = exe.with_extension("new");
    fs::copy(&new, &staged)?;
    if let Err(err) = fs::rename(&staged, exe) {
        if let Err(cleanup) = fs::remove_file(&staged) {
            warn!("Could not remove {}: {cleanup}", staged.display());
        }
        return Err(format!("rename over {}: {err}", exe.display()).into());
    }
    Ok(())
}

/// The whole bundle is replaced, the old one moved aside first so the running
/// app keeps a complete bundle until the new one is in place. A rename fails
/// when the temp dir sits on another volume, then the bundle is copied.
#[cfg(target_os = "macos")]
fn swap(dir: &Path, exe: &Path) -> Result<()> {
    let app = exe.ancestors().nth(3).ok_or("the install path is not inside a bundle")?;
    let new = dir.join("Alacritty.app");
    if !new.join("Contents").join("MacOS").join("alacritty").exists() {
        return Err("Alacritty.app is incomplete in the release tarball".into());
    }
    let old = old_path(app);
    clear_old(&old)?;
    fs::rename(app, &old)?;
    if fs::rename(&new, app).is_err() {
        let status = Command::new("cp").arg("-R").arg(&new).arg(app).status()?;
        if !status.success() {
            if let Err(restore) = fs::rename(&old, app) {
                warn!("Could not restore {}: {restore}", app.display());
            }
            return Err(format!("copy to {} failed", app.display()).into());
        }
    }
    if let Err(err) = fs::remove_dir_all(&old) {
        info!("{} stays until the next start: {err}", old.display());
    }
    Ok(())
}

/// Make room for the next `.old`. A window from the previous update may still
/// hold the last one, and a file in use can be renamed but not deleted, so it
/// is parked under a unique name for the next start to delete.
#[cfg(any(windows, target_os = "macos"))]
fn clear_old(old: &Path) -> Result<()> {
    if !old.exists() || remove(old).is_ok() {
        return Ok(());
    }
    let parked = old.with_extension(format!("old.{}", std::process::id()));
    fs::rename(old, &parked)?;
    Ok(())
}

fn remove(path: &Path) -> std::io::Result<()> {
    if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Delete the `.old` leftovers next to the install. One an older, still open
/// alacritty holds stays for that window's next start.
pub fn remove_old(exe: &Path) {
    for (dir, prefix) in old_prefixes(exe) {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with(&prefix) {
                continue;
            }
            let path = entry.path();
            match remove(&path) {
                Ok(()) => info!("Removed {}", path.display()),
                Err(err) => info!("{} is still in use: {err}", path.display()),
            }
        }
    }
}

/// Where the leftovers live and how their names start.
#[cfg(windows)]
fn old_prefixes(exe: &Path) -> Vec<(PathBuf, String)> {
    let bin_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    SWAPPED_FILES.iter().map(|name| (bin_dir.clone(), format!("{name}.old"))).collect()
}

#[cfg(target_os = "macos")]
fn old_prefixes(exe: &Path) -> Vec<(PathBuf, String)> {
    let Some(app) = exe.ancestors().nth(3) else { return Vec::new() };
    let (Some(parent), Some(name)) = (app.parent(), app.file_name()) else { return Vec::new() };
    vec![(parent.to_path_buf(), format!("{}.old", name.to_string_lossy()))]
}

#[cfg(not(any(windows, target_os = "macos")))]
fn old_prefixes(_exe: &Path) -> Vec<(PathBuf, String)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_line_is_found_by_name() {
        let sums = concat!(
            "0a1b  Alacritty-fork-0.17.0-5-macos.tar.gz\n",
            "C2D3 *Alacritty-fork-0.17.0-5-x86_64-windows.zip\n"
        );
        let mac = expected_sum(sums, "Alacritty-fork-0.17.0-5-macos.tar.gz");
        assert_eq!(mac.as_deref(), Some("0a1b"));
        let win = expected_sum(sums, "Alacritty-fork-0.17.0-5-x86_64-windows.zip");
        assert_eq!(win.as_deref(), Some("c2d3"));
        assert_eq!(expected_sum(sums, "SHA256SUMS"), None);
    }

    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn old_path_appends_to_the_full_name() {
        let old = old_path(Path::new("/bin/alacritty.exe"));
        assert_eq!(old.file_name().unwrap(), "alacritty.exe.old");
        let old = old_path(Path::new("/Applications/Alacritty.app"));
        assert_eq!(old.file_name().unwrap(), "Alacritty.app.old");
    }
}
