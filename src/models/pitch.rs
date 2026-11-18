//! Pitch-class names, shared by everything that writes a note's name: the MCP
//! units (`core/agent/units.rs`, C4 = 60) and the performance lane's keys
//! (Cubase-style, C3 = 60). Each adds its own octave convention.

/// The pitch-class names, sharps only (`360-mcp-server.md` decision 8).
pub(crate) const PITCH_CLASSES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// The pitch class of `pitch` as a name, sharps only: `A`, `C#`.
pub(crate) fn pitch_class_name(pitch: i32) -> &'static str {
    PITCH_CLASSES[pitch.rem_euclid(12) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_classes_are_sharps_from_c_in_every_octave() {
        assert_eq!(pitch_class_name(60), "C");
        assert_eq!(pitch_class_name(61), "C#");
        assert_eq!(pitch_class_name(71), "B");
        assert_eq!(pitch_class_name(-1), "B");
    }
}
