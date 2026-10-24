//! The settings modal's About tab: its links, the VST Compatible logo's
//! textures, and (macOS) the update check's progress, which the frame steps
//! (`Display::sync_update_check`). The app menu's About and Check for
//! Updates items open this tab (`view/appkit.rs`). Drawing and the hit-test
//! are `rendering/modals/settings.rs`. See `030-ui-design.md` § Settings
//! modal and `330-macos-app.md` § The About tab.

use std::cell::OnceCell;
use std::process::Command;

#[cfg(target_os = "macos")]
use crossbeam_channel::{Receiver, TryRecvError};
#[cfg(target_os = "macos")]
use egui::Key;
use egui::{ColorImage, Context, TextureHandle, TextureOptions, Vec2, vec2};

#[cfg(target_os = "macos")]
use crate::core::input_event::{InputEvent, KeyModifiers};
#[cfg(target_os = "macos")]
use crate::core::update_check::{UpdateCheck, start_update_check};
#[cfg(target_os = "macos")]
use crate::view::appkit::{AppMenuRequest, take_app_menu_request};

use super::Display;
#[cfg(target_os = "macos")]
use super::{Overlay, SettingsTab};

/// The logo's height in points, as `scripts/make-vst-logo.sh` renders it.
const VST_LOGO_HEIGHT: usize = 48;

/// The logo's raw RGBA, one per variant and scale, in [`VstLogo::textures`]
/// order: the standard (a black box) at 1x and 2x, then the negative (a white
/// box) at 1x and 2x.
const VST_LOGO_RGBA: [&[u8]; 4] = [
    include_bytes!("../../../docs/images/vst/VST_Compatible_Logo_Steinberg@1x.rgba"),
    include_bytes!("../../../docs/images/vst/VST_Compatible_Logo_Steinberg@2x.rgba"),
    include_bytes!("../../../docs/images/vst/VST_Compatible_Logo_Steinberg_negative@1x.rgba"),
    include_bytes!("../../../docs/images/vst/VST_Compatible_Logo_Steinberg_negative@2x.rgba"),
];

/// The logo's size in points: its 1x file's, whose width is read back from
/// the file's length.
pub(super) const VST_LOGO_SIZE: Vec2 = vec2(
    (VST_LOGO_RGBA[0].len() / (4 * VST_LOGO_HEIGHT)) as f32,
    VST_LOGO_HEIGHT as f32,
);

/// Stev's website.
const WEBSITE_URL: &str = "https://stevmidi.com";

/// Stev's repository.
const REPOSITORY_URL: &str = "https://github.com/stevmidi/stev";

/// A link the About tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AboutLink {
    /// stevmidi.com.
    Website,
    /// The GitHub repository.
    Repository,
    /// A newer release's page, once a check found one.
    #[cfg(target_os = "macos")]
    Release,
}

impl AboutLink {
    /// The links always shown, in their rows' order.
    pub(super) const LISTED: [AboutLink; 2] = [AboutLink::Website, AboutLink::Repository];

    /// The text a listed link reads as.
    pub(super) fn label(self) -> &'static str {
        match self {
            AboutLink::Website => "stevmidi.com",
            AboutLink::Repository => "github.com/stevmidi/stev",
            #[cfg(target_os = "macos")]
            AboutLink::Release => "View the release",
        }
    }
}

/// Where the update check stands.
#[cfg(target_os = "macos")]
#[derive(Default)]
pub(super) enum UpdateState {
    /// Not asked for since launch.
    #[default]
    Idle,
    /// Asked for; the next frame starts it.
    Requested,
    /// Running on the `"update-check"` thread; its answer arrives here.
    Checking(Receiver<UpdateCheck>),
    /// Answered.
    Done(UpdateCheck),
}

/// The VST Compatible logo's textures, each loaded the first time it is
/// drawn, in [`VST_LOGO_RGBA`] order.
#[derive(Default)]
pub(super) struct VstLogo {
    /// The loaded textures.
    textures: [OnceCell<TextureHandle>; 4],
}

impl VstLogo {
    /// The texture for a dark (`dark`) or light background at `hidpi` (2x)
    /// or 1x. On a dark background the standard logo's black box sits
    /// quietly; on a light one the negative's white box does.
    pub(super) fn texture(&self, ctx: &Context, dark: bool, hidpi: bool) -> &TextureHandle {
        let idx = 2 * usize::from(!dark) + usize::from(hidpi);
        self.textures[idx].get_or_init(|| {
            let scale = 1 + usize::from(hidpi);
            let rgba = VST_LOGO_RGBA[idx];
            let height = VST_LOGO_HEIGHT * scale;
            let image =
                ColorImage::from_rgba_unmultiplied([rgba.len() / (4 * height), height], rgba);
            ctx.load_texture("vst-compatible-logo", image, TextureOptions::LINEAR)
        })
    }
}

/// The About tab's state. Session-only.
#[derive(Default)]
pub(super) struct AboutTab {
    /// The update check's progress.
    #[cfg(target_os = "macos")]
    pub(super) update: UpdateState,
    /// The VST Compatible logo.
    pub(super) logo: VstLogo,
}

#[cfg(target_os = "macos")]
impl AboutTab {
    /// Whether a check is asked for or running — the button reads
    /// "Checking…" and takes no press.
    pub(super) fn is_checking(&self) -> bool {
        matches!(
            self.update,
            UpdateState::Requested | UpdateState::Checking(_)
        )
    }

    /// Asks for a check, unless one is already running.
    pub(super) fn request_check(&mut self) {
        if !self.is_checking() {
            self.update = UpdateState::Requested;
        }
    }

    /// The page of the newer release a check found.
    fn release_url(&self) -> Option<&str> {
        match &self.update {
            UpdateState::Done(UpdateCheck::Available { url, .. }) => Some(url),
            _ => None,
        }
    }
}

/// Opens `url` in the default browser. The opener is left to finish on its
/// own: it hands the URL over and exits at once.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    const OPENER: &str = "open";
    #[cfg(target_os = "windows")]
    const OPENER: &str = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    const OPENER: &str = "xdg-open";
    if let Err(err) = Command::new(OPENER).arg(url).spawn() {
        eprintln!("couldn't open {url}: {err}");
    }
}

impl Display {
    /// Opens `link` in the browser.
    pub(super) fn open_about_link(&self, link: AboutLink) {
        let url = match link {
            AboutLink::Website => WEBSITE_URL,
            AboutLink::Repository => REPOSITORY_URL,
            #[cfg(target_os = "macos")]
            AboutLink::Release => match self.settings.about.release_url() {
                Some(url) => url,
                None => return,
            },
        };
        open_in_browser(url);
    }

    /// Once a frame: starts an asked-for update check, and takes a running
    /// one's answer when it arrives. The check wakes the frame itself.
    #[cfg(target_os = "macos")]
    pub(super) fn sync_update_check(&mut self, ctx: &Context) {
        let update = &mut self.settings.about.update;
        match update {
            UpdateState::Requested => {
                *update = UpdateState::Checking(start_update_check(ctx.clone()))
            }
            UpdateState::Checking(rx) => match rx.try_recv() {
                Ok(result) => *update = UpdateState::Done(result),
                Err(TryRecvError::Disconnected) => *update = UpdateState::Done(UpdateCheck::Failed),
                Err(TryRecvError::Empty) => {}
            },
            UpdateState::Idle | UpdateState::Done(_) => {}
        }
    }

    /// Opens Settings on `tab`, or switches to it when Settings is already
    /// up — the app menu's way in.
    #[cfg(target_os = "macos")]
    fn show_settings_tab(&mut self, tab: SettingsTab) {
        if self.overlay != Some(Overlay::Settings) {
            self.open_settings();
        }
        self.settings.tab = tab;
    }

    /// Once a frame, ahead of the frame's own events: acts on the app menu's
    /// About Stev, Check for Updates… and Settings… (`view/appkit.rs`). About
    /// opens Settings on the About tab; Check for Updates does too and starts
    /// a check, unless one is running. Settings… clicked opens Settings on the
    /// tab last used; its `⌘,` arrives as the key itself, pushed onto
    /// `events`, so the menu changes nothing about the chord. While a project
    /// dialog, a field or a non-settings overlay has the keyboard, a clicked
    /// item does nothing — the menu can't end those.
    #[cfg(target_os = "macos")]
    pub(super) fn take_app_menu_request(&mut self, events: &mut Vec<InputEvent>) {
        let Some(request) = take_app_menu_request() else {
            return;
        };
        if request == AppMenuRequest::SettingsKey {
            let modifiers = KeyModifiers {
                command: true,
                ..KeyModifiers::default()
            };
            let key = Key::Comma;
            events.insert(0, InputEvent::KeyPressed { key, modifiers });
            return;
        }
        if self.keyboard_taken() && self.overlay != Some(Overlay::Settings) {
            return;
        }
        match request {
            AppMenuRequest::About => self.show_settings_tab(SettingsTab::About),
            AppMenuRequest::CheckForUpdates => {
                self.show_settings_tab(SettingsTab::About);
                self.settings.about.request_check();
            }
            AppMenuRequest::Settings => {
                if self.overlay != Some(Overlay::Settings) {
                    self.open_settings();
                }
            }
            AppMenuRequest::SettingsKey => {}
        }
    }
}
