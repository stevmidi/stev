//! Saving and restoring a VST3 plugin's preset.
//!
//! VST3 splits state in two, and a host is expected to keep **both**:
//!
//! - `IComponent::getState` — the processor's state. This is the sound.
//! - `IEditController::getState` — the controller's own state, which is
//!   whatever the UI wants to remember that isn't a parameter (the open page, a
//!   zoom level, a browser filter).
//!
//! On restore the order matters and is fixed by the spec: the component is
//! given its state, then the *same bytes* are handed to
//! `IEditController::setComponentState` so the UI reflects the sound, and only
//! then does the controller get its own state back. Skipping the middle step is
//! how a plugin ends up making the right sound while showing the wrong values.
//!
//! Both halves go into one blob so the rest of Stev keeps seeing a plugin's
//! state as a single opaque `Vec<u8>` — the same shape CLAP's state blob has,
//! stored in the same base64 field of the `.stev`. See `060-persistence.md` and
//! `docs/180-vst3-host.md`.

use vst3::ComPtr;
use vst3::ComWrapper;
use vst3::Steinberg::Vst::{IComponent, IComponentTrait, IEditController, IEditControllerTrait};
use vst3::Steinberg::{IBStream, kResultOk};

use super::stream::MemoryStream;

/// Identifies a Stev VST3 state blob. Present so a blob from some other
/// source — or a CLAP blob that somehow reached a VST3 track — is rejected
/// rather than fed to a plugin as if it were a preset.
const MAGIC: &[u8; 4] = b"MSV3";

/// Container format version. Bumped if the layout below ever changes; an
/// unrecognised version is refused, which costs the preset rather than risking
/// a misparse.
const VERSION: u8 = 1;

/// Bytes before the first payload: [`MAGIC`], [`VERSION`], component length.
const HEADER_LEN: usize = MAGIC.len() + 1 + 4;

/// Packs the two halves into one blob.
fn encode(component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + component.len() + 4 + controller.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(component.len() as u32).to_le_bytes());
    out.extend_from_slice(component);
    out.extend_from_slice(&(controller.len() as u32).to_le_bytes());
    out.extend_from_slice(controller);
    out
}

/// Unpacks a blob into `(component, controller)`.
///
/// `None` for anything that isn't a well-formed blob of a version we
/// understand — a truncated file, a foreign format, a future version. The
/// caller then loads the plugin at its defaults, which is the right failure:
/// a wrong preset is worse than no preset.
fn decode(bytes: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC || bytes[4] != VERSION {
        return None;
    }
    let component_len = u32::from_le_bytes(bytes[5..9].try_into().ok()?) as usize;
    let component_end = HEADER_LEN.checked_add(component_len)?;
    // The controller length field must itself fit.
    let controller_len_end = component_end.checked_add(4)?;
    if bytes.len() < controller_len_end {
        return None;
    }
    let controller_len =
        u32::from_le_bytes(bytes[component_end..controller_len_end].try_into().ok()?) as usize;
    let controller_end = controller_len_end.checked_add(controller_len)?;
    if bytes.len() < controller_end {
        return None;
    }
    Some((
        bytes[HEADER_LEN..component_end].to_vec(),
        bytes[controller_len_end..controller_end].to_vec(),
    ))
}

/// The edit controller, if it has state of its own that must be saved and
/// restored separately from the component's — i.e. only a *separate*
/// controller.
///
/// **`capture` and `apply` must agree on this**, which is why it is one
/// function rather than a condition written twice. If capture stores a
/// controller half that apply ignores the blob merely grows; if apply restores
/// one that capture never meant as separate, the plugin's own state is applied
/// twice over.
fn separate_controller<C>(controller: Option<C>, controller_is_component: bool) -> Option<C> {
    controller.filter(|_| !controller_is_component)
}

/// Captures a plugin's current state, ready to persist.
///
/// `None` when the component declines to report state at all, in which case the
/// caller keeps whatever blob it already had rather than replacing it with
/// nothing. A controller that declines is not a failure — plenty have nothing
/// of their own to save — and yields an empty second half.
///
/// # Safety
///
/// Both must be live, initialised objects, and this must run on the main
/// thread.
pub(super) unsafe fn capture(
    component: &ComPtr<IComponent>,
    controller: Option<&ComPtr<IEditController>>,
    controller_is_component: bool,
) -> Option<Vec<u8>> {
    // SAFETY: the caller guarantees live objects on the main thread. Each
    // stream outlives the call it is passed to, and its bytes are taken back
    // only after the plugin has finished writing.
    unsafe {
        let component_state = write_state(|stream| component.getState(stream))?;
        // Only a *separate* controller has state of its own worth keeping. On a
        // single-component plugin `IEditController::getState` is the very same
        // method on the very same object, so capturing it stores the plugin's
        // state twice, doubling the blob in the project file and setting up a
        // redundant apply on restore.
        let controller_state = separate_controller(controller, controller_is_component)
            .and_then(|c| write_state(|stream| c.getState(stream)))
            .unwrap_or_default();
        Some(encode(&component_state, &controller_state))
    }
}

/// Restores a plugin's state from a blob produced by [`capture`].
///
/// Must run **before** the component is activated. Returns whether anything was
/// applied; a blob that fails to decode leaves the plugin at its defaults.
///
/// # Safety
///
/// Both must be live, initialised, not-yet-activated objects, on the main
/// thread.
pub(super) unsafe fn apply(
    component: &ComPtr<IComponent>,
    controller: Option<&ComPtr<IEditController>>,
    controller_is_component: bool,
    bytes: &[u8],
) -> bool {
    let Some((component_state, controller_state)) = decode(bytes) else {
        dprintln!("vst3: ignoring a state blob that is not ours or is truncated");
        return false;
    };

    let component_stream = ComWrapper::new(MemoryStream::from_bytes(component_state));
    let Some(component_ptr) = component_stream
        .as_com_ref::<IBStream>()
        .map(|r| r.as_ptr())
    else {
        return false;
    };

    // SAFETY: the caller guarantees live, inactive objects on the main thread.
    // Both streams outlive every call they are handed to.
    unsafe {
        component.setState(component_ptr);

        // On a single-component plugin the controller *is* the component, so
        // `setState` above has already told the UI everything. Calling
        // `setComponentState` and `setState` on it as well would apply the
        // plugin's own state a second and third time, which the spec does not
        // ask for and no plugin should have to tolerate.
        if let Some(controller) = separate_controller(controller, controller_is_component) {
            // The *same bytes*, from the start, so the UI ends up showing what
            // the processor is actually doing. Rewinding is not optional: the
            // plugin's `setState` will have left the cursor at the end, and a
            // controller handed an exhausted stream silently restores nothing.
            component_stream.rewind();
            controller.setComponentState(component_ptr);

            if !controller_state.is_empty() {
                let controller_stream = ComWrapper::new(MemoryStream::from_bytes(controller_state));
                if let Some(ptr) = controller_stream
                    .as_com_ref::<IBStream>()
                    .map(|r| r.as_ptr())
                {
                    controller.setState(ptr);
                }
            }
        }
    }
    true
}

/// Runs `write` against a fresh stream and returns what it wrote, or `None` if
/// the plugin reported failure.
///
/// # Safety
///
/// `write` must do nothing with the stream pointer beyond the call.
unsafe fn write_state(write: impl FnOnce(*mut IBStream) -> i32) -> Option<Vec<u8>> {
    let stream = ComWrapper::new(MemoryStream::empty());
    let ptr = stream.as_com_ref::<IBStream>()?.as_ptr();
    if write(ptr) != kResultOk {
        return None;
    }
    Some(stream.take_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_separate_controller_has_state_of_its_own() {
        // Dual-component: the controller is a different object with its own
        // state, so both halves are real.
        assert_eq!(
            separate_controller(Some("controller"), false),
            Some("controller")
        );
        // Single-component: `IEditController::setState` *is*
        // `IComponent::setState` on the same object, so treating it separately
        // applies the plugin's state twice over and doubles the stored blob.
        assert_eq!(separate_controller(Some("controller"), true), None);
        // No controller at all.
        assert_eq!(separate_controller(None::<&str>, false), None);
    }

    #[test]
    fn a_blob_round_trips_both_halves() {
        let blob = encode(b"component", b"controller");
        assert_eq!(
            decode(&blob),
            Some((b"component".to_vec(), b"controller".to_vec()))
        );
    }

    #[test]
    fn an_empty_controller_half_is_normal_and_round_trips() {
        // Plenty of plugins have no controller-only state to save.
        let blob = encode(b"component", b"");
        assert_eq!(decode(&blob), Some((b"component".to_vec(), Vec::new())));
    }

    #[test]
    fn both_halves_empty_still_decodes() {
        assert_eq!(decode(&encode(b"", b"")), Some((Vec::new(), Vec::new())));
    }

    #[test]
    fn binary_state_survives_verbatim() {
        // Plugin state is arbitrary bytes, including NULs and high bytes.
        let component: Vec<u8> = (0u8..=255).collect();
        let controller = vec![0u8, 0, 0, 255, 128];
        let blob = encode(&component, &controller);
        assert_eq!(decode(&blob), Some((component, controller)));
    }

    #[test]
    fn a_foreign_blob_is_refused_rather_than_fed_to_a_plugin() {
        // A CLAP state blob reaching a VST3 track, or any other stray bytes.
        assert_eq!(decode(b"not ours at all"), None);
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"MSV"), None);
    }

    #[test]
    fn a_blob_from_another_container_version_is_refused() {
        let mut blob = encode(b"x", b"y");
        blob[4] = VERSION.wrapping_add(1);
        assert_eq!(decode(&blob), None);
    }

    #[test]
    fn a_truncated_blob_is_refused_at_every_cut_point() {
        // Every prefix of a valid blob must be rejected, not half-read.
        let blob = encode(b"component", b"controller");
        for cut in 0..blob.len() {
            assert_eq!(decode(&blob[..cut]), None, "prefix of length {cut}");
        }
        assert!(decode(&blob).is_some());
    }

    #[test]
    fn a_lying_length_field_cannot_read_past_the_buffer() {
        let mut blob = encode(b"short", b"");
        // Claim the component half is enormous.
        blob[5..9].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&blob), None);
    }
}
