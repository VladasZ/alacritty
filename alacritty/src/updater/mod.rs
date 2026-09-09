//! Self update from the fork's GitHub releases.
//!
//! A release build carries its fork tag, like `fork-0.17.0-6`, baked in by CI.
//! A few seconds after launch a thread asks GitHub for the newest release and,
//! when it is newer, the tab strip shows an update chip. A click on the chip
//! downloads the asset for this machine, checks it against `SHA256SUMS`,
//! unpacks it and swaps the installed files. The running terminal keeps its
//! shells, the new build starts with the next alacritty process.

mod install;

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use alacritty_terminal::thread::spawn_named;
use log::{error, info, warn};
use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType};

// The redirect target names the newest tag. Unlike the GitHub API this has
// no rate limit per IP, which an office NAT shares between everyone.
const LATEST_RELEASE_URL: &str = "https://github.com/VladasZ/alacritty/releases/latest";
const RELEASE_DOWNLOAD_URL: &str = "https://github.com/VladasZ/alacritty/releases/download";
const SUMS_ASSET: &str = "SHA256SUMS";

/// Delay before the launch check, so startup is not slowed down.
const CHECK_DELAY: Duration = Duration::from_secs(3);

/// The fork tag this binary was built as, `None` for a dev build.
pub fn current_tag() -> Option<&'static str> {
    option_env!("ALACRITTY_FORK_TAG")
}

/// Where the update stands, shown as a chip in the tab strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Available(UpdateRelease),
    /// Whole percent of the asset download.
    Downloading(u8),
    Unpacking,
    Installing,
    /// The new build is on disk, this is its tag.
    Restart(String),
    Failed,
}

impl UpdateState {
    /// Text of the chip, `None` when there is nothing to show.
    pub fn label(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Available(release) => Some(format!("Update {}", release.tag)),
            Self::Downloading(percent) => Some(format!("Downloading {percent}%")),
            Self::Unpacking => Some("Unpacking...".into()),
            Self::Installing => Some("Installing...".into()),
            Self::Restart(_) => Some("Restart to update".into()),
            Self::Failed => Some("Update failed".into()),
        }
    }

    /// A click on the chip starts the install only while one is offered.
    pub fn clickable(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    pub fn busy(&self) -> bool {
        matches!(self, Self::Downloading(_) | Self::Unpacking | Self::Installing)
    }
}

/// A newer release with the asset this machine installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRelease {
    pub tag: String,
    asset_name: String,
    asset_url: String,
    /// Bytes of the asset, for the download percent and the size check.
    size: u64,
    sums_url: String,
}

/// Clean up after the last update and start the launch check. Does nothing
/// for a dev build.
pub fn start(proxy: EventLoopProxy<Event>) {
    let Some((tag, exe)) = installed_release() else {
        info!("Not a release build at the install path, no self update");
        return;
    };
    install::remove_old(&exe);
    spawn_named("update check", move || {
        thread::sleep(CHECK_DELAY);
        match latest_release(tag) {
            Ok(Some(release)) => {
                info!("Update {} is available, running {tag}", release.tag);
                send(&proxy, UpdateState::Available(release));
            },
            Ok(None) => info!("Running the newest release {tag}"),
            // Offline is normal for a terminal, nothing to show.
            Err(err) => info!("Update check skipped: {err}"),
        }
    });
}

/// Download and install the release in the background. The outcome lands as
/// an update event, a failure also as an error in the message bar.
pub fn spawn_install(release: UpdateRelease, proxy: EventLoopProxy<Event>) {
    let Some((_, exe)) = installed_release() else { return };
    spawn_named("update install", move || {
        let tag = release.tag.clone();
        let report = |state| send(&proxy, state);
        match install::run(&release, &exe, &report) {
            Ok(()) => {
                info!("Installed {tag}, restart alacritty to use it");
                send(&proxy, UpdateState::Restart(tag));
            },
            Err(err) => {
                error!("Update to {tag} failed: {err}");
                send(&proxy, UpdateState::Failed);
            },
        }
    });
}

fn send(proxy: &EventLoopProxy<Event>, state: UpdateState) {
    if proxy.send_event(Event::new(EventType::Update(state), None)).is_err() {
        warn!("Update state dropped, the event loop is gone");
    }
}

/// The exe thing installs on this machine.
fn install_exe() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(PathBuf::from("/Applications/Alacritty.app/Contents/MacOS/alacritty"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let name = if cfg!(windows) { "alacritty.exe" } else { "alacritty" };
        Some(home::home_dir()?.join(".local").join("bin").join(name))
    }
}

/// The tag and exe of a release build running from the install path. A dev
/// build has no tag and runs from `target/`, it must never touch the daily
/// terminal's files.
fn installed_release() -> Option<(&'static str, PathBuf)> {
    let tag = current_tag()?;
    let exe = install_exe()?;
    let running = std::env::current_exe().ok()?.canonicalize().ok()?;
    (running == exe.canonicalize().ok()?).then_some((tag, exe))
}

fn latest_release(current: &str) -> install::Result<Option<UpdateRelease>> {
    let headers = install::curl_headers(LATEST_RELEASE_URL, false)?;
    let tag = tag_from_redirect(&headers).ok_or("no redirect to the newest release")?;
    if !is_newer(&tag, current) {
        return Ok(None);
    }
    let asset_name = asset_name(&tag)
        .ok_or_else(|| format!("no release asset for {}", std::env::consts::ARCH))?;
    let asset_url = format!("{RELEASE_DOWNLOAD_URL}/{tag}/{asset_name}");
    // The asset redirects to the object store, the size is on the final hop.
    let size = header(&install::curl_headers(&asset_url, true)?, "content-length")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("no size for {asset_url}"))?;
    Ok(Some(UpdateRelease {
        sums_url: format!("{RELEASE_DOWNLOAD_URL}/{tag}/{SUMS_ASSET}"),
        tag,
        asset_name,
        asset_url,
        size,
    }))
}

/// The tag `releases/latest` redirects to, the last segment of its location.
fn tag_from_redirect(headers: &str) -> Option<String> {
    let location = header(headers, "location")?;
    let (_, tag) = location.rsplit_once("/releases/tag/")?;
    (!tag.is_empty()).then(|| tag.to_string())
}

/// The value of the last `name` header, so a followed redirect reports the
/// final response.
fn header(headers: &str, name: &str) -> Option<String> {
    headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().to_string())
        .next_back()
}

/// Numbers of a `fork-<major>.<minor>.<patch>-<build>` tag.
fn tag_numbers(tag: &str) -> Option<[u64; 4]> {
    let (version, build) = tag.strip_prefix("fork-")?.rsplit_once('-')?;
    let mut parts = version.split('.');
    let mut numbers = [0; 4];
    for number in numbers.iter_mut().take(3) {
        *number = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    numbers[3] = build.parse().ok()?;
    Some(numbers)
}

/// A tag that does not follow the scheme is never offered.
fn is_newer(latest: &str, current: &str) -> bool {
    match (tag_numbers(latest), tag_numbers(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// The release asset for this machine, the names the release workflow writes.
fn asset_name(tag: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some(format!("Alacritty-{tag}-macos.tar.gz"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let arch = std::env::consts::ARCH;
        if arch != "x86_64" && arch != "aarch64" {
            return None;
        }
        let os = if cfg!(windows) { "windows.zip" } else { "linux.tar.gz" };
        Some(format!("Alacritty-{tag}-{arch}-{os}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_numbers_follow_the_fork_scheme() {
        assert_eq!(tag_numbers("fork-0.17.0-5"), Some([0, 17, 0, 5]));
        assert_eq!(tag_numbers("fork-1.2.3-12"), Some([1, 2, 3, 12]));
        assert_eq!(tag_numbers("v0.17.0"), None);
        assert_eq!(tag_numbers("fork-0.17.0"), None);
        assert_eq!(tag_numbers("fork-0.17-5"), None);
        assert_eq!(tag_numbers("fork-0.17.0.1-5"), None);
        assert_eq!(tag_numbers("fork-0.17.0-x"), None);
    }

    #[test]
    fn newer_compares_version_then_build() {
        assert!(is_newer("fork-0.17.0-6", "fork-0.17.0-5"));
        assert!(is_newer("fork-0.17.0-10", "fork-0.17.0-9"));
        assert!(is_newer("fork-0.18.0-1", "fork-0.17.0-9"));
        assert!(!is_newer("fork-0.17.0-5", "fork-0.17.0-5"));
        assert!(!is_newer("fork-0.17.0-4", "fork-0.17.0-5"));
        assert!(!is_newer("v0.18.0", "fork-0.17.0-5"));
    }

    #[test]
    fn asset_name_matches_the_release_workflow() {
        let name = asset_name("fork-0.17.0-5").unwrap();
        assert!(name.starts_with("Alacritty-fork-0.17.0-5-"));
        let suffix = if cfg!(target_os = "macos") {
            "macos.tar.gz".to_string()
        } else if cfg!(windows) {
            format!("{}-windows.zip", std::env::consts::ARCH)
        } else {
            format!("{}-linux.tar.gz", std::env::consts::ARCH)
        };
        assert!(name.ends_with(&suffix), "{name}");
    }

    #[test]
    fn tag_comes_from_the_latest_redirect() {
        let headers = concat!(
            "HTTP/1.1 302 Found\r\n",
            "Location: https://github.com/VladasZ/alacritty/releases/tag/fork-0.17.0-5\r\n",
            "\r\n"
        );
        assert_eq!(tag_from_redirect(headers).as_deref(), Some("fork-0.17.0-5"));
        assert_eq!(tag_from_redirect("HTTP/1.1 200 OK\r\n"), None);
    }

    #[test]
    fn header_takes_the_final_hop() {
        let headers = concat!(
            "HTTP/1.1 302 Found\r\n",
            "Content-Length: 0\r\n",
            "location: https://objects.example.com/asset\r\n",
            "\r\n",
            "HTTP/1.1 200 OK\r\n",
            "content-length: 3012518\r\n",
            "\r\n"
        );
        assert_eq!(header(headers, "Content-Length").as_deref(), Some("3012518"));
        assert_eq!(header(headers, "etag"), None);
    }
}
