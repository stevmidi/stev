//! The About tab's Check for updates: asks GitHub for Stev's latest release
//! and compares its tag with the running version. The fetch runs
//! `/usr/bin/curl` on a short-lived `"update-check"` thread (macOS, where the
//! app ships), only when asked; never on the audio or render path. The
//! comparison and the response parsing are pure. See
//! `archive/330-macos-app.md` § The About tab.
//!
//! It builds everywhere, but the About tab offers it only on macOS
//! (`about_tab.rs`, `UPDATE_CHECK`).

use std::{process::Command, thread};

use crossbeam_channel::{Receiver, bounded};
use egui::Context;
use log::warn;
use serde::Deserialize;

/// GitHub's API for the newest published release (drafts and pre-releases
/// left out).
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/stevmidi/stev/releases/latest";

/// The running version, `Cargo.toml`'s.
pub(crate) const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateCheck {
    /// The running version is the latest, or newer (a build from `main`).
    UpToDate,
    /// A newer release exists.
    Available {
        /// Its version, without the tag's `v`.
        version: String,
        /// Its release page.
        url: String,
    },
    /// GitHub couldn't be reached, or answered with something unreadable —
    /// never read as a newer version.
    Failed,
}

/// The fields read from GitHub's release object.
#[derive(Deserialize)]
struct Release {
    /// The release's tag, `v0.6.0`.
    tag_name: String,
    /// The release's page.
    html_url: String,
}

/// `major.minor.patch` of a version or tag, a leading `v` allowed. `None` for
/// anything else, a pre-release suffix included.
fn parse_version(tag: &str) -> Option<[u64; 3]> {
    let tag = tag.trim();
    let mut parts = tag.strip_prefix('v').unwrap_or(tag).split('.');
    let mut version = [0; 3];
    for part in &mut version {
        *part = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(version)
}

/// The check's answer for the running version `current` from GitHub's
/// `response` body (`None` when the fetch failed).
pub(crate) fn update_check_result(current: &str, response: Option<&str>) -> UpdateCheck {
    let Some(release) = response.and_then(|body| serde_json::from_str::<Release>(body).ok()) else {
        return UpdateCheck::Failed;
    };
    let (Some(latest), Some(running)) = (parse_version(&release.tag_name), parse_version(current))
    else {
        return UpdateCheck::Failed;
    };
    if latest > running {
        let [major, minor, patch] = latest;
        UpdateCheck::Available {
            version: format!("{major}.{minor}.{patch}"),
            url: release.html_url,
        }
    } else {
        UpdateCheck::UpToDate
    }
}

/// Starts a check on an `"update-check"` thread and returns where its one
/// answer arrives; `ctx` is woken when it does. A thread that can't start
/// drops its sender, which the receiver reads as a failed check.
pub(crate) fn start_update_check(ctx: Context) -> Receiver<UpdateCheck> {
    let (tx, rx) = bounded(1);
    let spawned = thread::Builder::new()
        .name("update-check".to_string())
        .spawn(move || {
            let body = Command::new("/usr/bin/curl")
                .args(["-fsSL", "--max-time", "10"])
                .args(["-H", "Accept: application/vnd.github+json"])
                .args(["-A", &format!("Stev/{CURRENT_VERSION}")])
                .arg(LATEST_RELEASE_URL)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok());
            tx.send(update_check_result(CURRENT_VERSION, body.as_deref()))
                .ok();
            ctx.request_repaint();
        });
    if let Err(err) = spawned {
        warn!("couldn't start the update check: {err}");
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::{UpdateCheck, parse_version, update_check_result};

    /// A release body as GitHub sends it, cut to what matters.
    fn release(tag: &str) -> String {
        format!(
            r#"{{"tag_name": "{tag}", "html_url": "https://github.com/stevmidi/stev/releases/tag/{tag}", "draft": false}}"#
        )
    }

    #[test]
    fn a_version_reads_with_or_without_its_v() {
        assert_eq!(parse_version("v0.5.0"), Some([0, 5, 0]));
        assert_eq!(parse_version("1.12.3"), Some([1, 12, 3]));
        assert_eq!(parse_version(" v2.0.10\n"), Some([2, 0, 10]));
    }

    #[test]
    fn anything_but_three_numbers_is_no_version() {
        assert_eq!(parse_version("v0.5"), None);
        assert_eq!(parse_version("v0.5.0.1"), None);
        assert_eq!(parse_version("v0.6.0-beta"), None);
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn a_newer_tag_is_an_update_with_its_page() {
        assert_eq!(
            update_check_result("0.5.0", Some(&release("v0.6.0"))),
            UpdateCheck::Available {
                version: "0.6.0".to_owned(),
                url: "https://github.com/stevmidi/stev/releases/tag/v0.6.0".to_owned(),
            }
        );
    }

    #[test]
    fn versions_compare_as_numbers_not_text() {
        assert!(matches!(
            update_check_result("0.9.0", Some(&release("v0.10.0"))),
            UpdateCheck::Available { .. }
        ));
        assert_eq!(
            update_check_result("0.10.0", Some(&release("v0.9.0"))),
            UpdateCheck::UpToDate
        );
    }

    #[test]
    fn the_same_or_an_older_tag_is_up_to_date() {
        assert_eq!(
            update_check_result("0.5.0", Some(&release("v0.5.0"))),
            UpdateCheck::UpToDate
        );
        assert_eq!(
            update_check_result("0.6.0", Some(&release("v0.5.9"))),
            UpdateCheck::UpToDate
        );
    }

    #[test]
    fn an_unreadable_answer_is_a_failed_check_never_an_update() {
        assert_eq!(update_check_result("0.5.0", None), UpdateCheck::Failed);
        assert_eq!(
            update_check_result("0.5.0", Some("<html>rate limited</html>")),
            UpdateCheck::Failed
        );
        assert_eq!(
            update_check_result("0.5.0", Some(r#"{"message": "Not Found"}"#)),
            UpdateCheck::Failed
        );
        assert_eq!(
            update_check_result("0.5.0", Some(&release("nightly"))),
            UpdateCheck::Failed
        );
    }
}
