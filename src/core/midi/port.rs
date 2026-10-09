//! Shared MIDI port-name helpers used by both the input and output sides.

#[cfg(target_os = "macos")]
use midir::MidiInput;

/// Name of the virtual port Stev registers on Unix (CoreMIDI / ALSA) so
/// other apps can send to / receive from the sequencer without a hardware
/// device. Excluded from the pickable port lists (it's our own endpoint) and
/// re-inserted at the top as an explicit choice.
pub(crate) const VIRTUAL_PORT_NAME: &str = "Virtual: Stev";

/// Strips the trailing ALSA client:port suffix (` XX:X`) that changes across reboots,
/// e.g. `"MicroLab mk3:MicroLab mk3 32:0"` → `"MicroLab mk3:MicroLab mk3"`.
pub(crate) fn port_base_name(name: &str) -> &str {
    if let Some(last_space) = name.rfind(' ') {
        let suffix = &name[last_space + 1..];
        let mut parts = suffix.splitn(2, ':');
        if parts
            .next()
            .is_some_and(|p| p.chars().all(|c| c.is_ascii_digit()))
            && parts
                .next()
                .is_some_and(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            return &name[..last_space];
        }
    }
    name
}

/// The user-pickable port list from a `midir` side's port names: our own
/// endpoint left out (the virtual port on the *other* side registers a real
/// port under [`VIRTUAL_PORT_NAME`]), and on Unix the virtual port put back
/// first as an explicit choice. For the settings modal.
pub(crate) fn pickable_port_names(names: impl Iterator<Item = String>) -> Vec<String> {
    let mut ports = Vec::new();
    #[cfg(unix)]
    ports.push(VIRTUAL_PORT_NAME.to_string());
    ports.extend(names.filter(|n| n != VIRTUAL_PORT_NAME));
    ports
}

/// Index of the port in `ports` that is `name` up to its ALSA client:port
/// suffix ([`port_base_name`]), `None` with no `name` or no match.
pub(crate) fn matching_port_index(ports: &[String], name: Option<&str>) -> Option<usize> {
    let name = port_base_name(name?);
    ports.iter().position(|port| port_base_name(port) == name)
}

/// Creates the process's first CoreMIDI client, to be called on the main
/// thread before any MIDI thread starts and kept for the session. CoreMIDI
/// keeps a process's device list current through the run loop of the thread
/// that created its first client, and only the main thread runs one
/// (eframe's). If a MIDI thread got there first, a device plugged in after
/// launch would never appear in any enumeration — no hot-plug, a stale modal.
#[cfg(target_os = "macos")]
pub(crate) fn anchor_coremidi_to_main_thread() -> Option<MidiInput> {
    MidiInput::new("Stev notifications").ok()
}

#[cfg(test)]
mod tests {
    use super::{VIRTUAL_PORT_NAME, matching_port_index, pickable_port_names};

    #[test]
    fn pickable_port_names_drops_our_endpoint_and_lists_the_virtual_port_first() {
        let names = ["Synth", VIRTUAL_PORT_NAME, "Keys"].map(String::from);
        let ports = pickable_port_names(names.into_iter());
        let expected: &[&str] = if cfg!(unix) {
            &[VIRTUAL_PORT_NAME, "Synth", "Keys"]
        } else {
            &["Synth", "Keys"]
        };
        assert_eq!(ports, expected);
    }

    #[test]
    fn matching_port_index_ignores_the_alsa_suffix() {
        let ports = vec!["Other 20:0".to_string(), "MicroLab mk3 32:0".to_string()];
        assert_eq!(
            matching_port_index(&ports, Some("MicroLab mk3 24:0")),
            Some(1)
        );
        assert_eq!(matching_port_index(&ports, Some("Missing")), None);
        assert_eq!(matching_port_index(&ports, None), None);
    }
}
