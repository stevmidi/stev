//! VST3's half of the plugin catalog: enumerate the instrument classes each
//! installed `.vst3` bundle's factory exposes. The bundle walk itself is shared
//! with every other format — see [`catalog`](crate::core::plugin_host::catalog).
//!
//! A VST3 module's factory lists *classes*, not plugins, and every kind of
//! class is in the same list — audio effects, instruments, and the separate
//! edit-controller classes that pair with them. Two filters narrow that to what
//! the Track modal should offer: the class category must be
//! [`AUDIO_MODULE_CLASS`], and its sub-categories must name `Instrument`.
//!
//! Class identity is a 16-byte UID rather than CLAP's string id, so
//! `PluginCatalogEntry::plugin_id` carries it as hex — see [`uid_to_hex`].

use std::ffi::c_char;
use std::path::Path;

use vst3::ComPtr;
use vst3::Steinberg::{
    IPluginFactory, IPluginFactory2Trait, IPluginFactory3Trait, IPluginFactoryTrait, PClassInfo,
    PClassInfo2, PClassInfoW, TUID, kResultOk,
};
use vst3::Steinberg::{IPluginFactory2, IPluginFactory3};

use crate::core::plugin_host::catalog::{PluginCatalogEntry, PluginFormat, installed_bundles};

use super::cache::{CachedClass, ScanCache};
use super::child::scan_bundle_out_of_process;
use super::module::with_module;

/// The `PClassInfo::category` value marking a class as an audio processing
/// module — an effect or an instrument, as opposed to an edit controller or
/// any of the other class kinds a factory may list. `kVstAudioEffectClass` in
/// the C++ headers, which the generated bindings don't carry (it is a `#define`
/// of a plain string literal).
const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// The sub-category token marking an audio module as an instrument rather than
/// an effect. `PClassInfo2::subCategories` is a `|`-separated list, so this is
/// matched against the split tokens — an exact token compare, not a substring
/// search, so `Instrument|Synth` matches and a hypothetical `NotAnInstrument`
/// would not.
const INSTRUMENT_SUBCATEGORY: &str = "Instrument";

/// One class's identity and display name, whichever of the three
/// `getClassInfo*` calls it came from.
struct ClassInfo {
    /// The class UID.
    cid: TUID,
    /// `PClassInfo::category`.
    category: String,
    /// Human-readable class name.
    name: String,
    /// `|`-separated sub-category list. Empty for a factory that only offers
    /// the original [`IPluginFactory`], which
    /// has no sub-categories to report.
    sub_categories: String,
}

/// Every installed `.vst3` bundle's instrument classes, as catalog entries.
///
/// Each bundle is either a **cache hit** (its modification time is unchanged
/// since it was last scanned, so its remembered classes are reused) or a
/// **child-process scan** — see [`child`](super::child) for why scanning cannot
/// happen in this process at all. The cache is what makes this cheap after the
/// first run: a cold scan of ~60 bundles takes the better part of a minute, a
/// warm one is a single file read.
///
/// A bundle whose scan failed is remembered as failed and skipped until the
/// plugin changes, so one bad plugin costs its own absence rather than a crash
/// or a minute of every launch. Sorting across formats is the shared
/// [`scan_catalog`](crate::core::plugin_host::catalog::scan_catalog)'s job.
pub(super) fn scan_catalog() -> Vec<PluginCatalogEntry> {
    let bundles = installed_bundles(PluginFormat::Vst3);
    let mut cache = ScanCache::load();
    cache.retain_installed(&bundles);

    let mut out = Vec::new();
    for bundle in &bundles {
        let classes = match cache.get(bundle) {
            Some(cached) => {
                if cached.failed {
                    dprintln!(
                        "vst3: skipping {} — a previous scan of it failed",
                        bundle.display()
                    );
                    continue;
                }
                cached.classes.clone()
            }
            None => match scan_bundle_out_of_process(bundle) {
                Ok(classes) => {
                    cache.insert(bundle, classes.clone(), false);
                    classes
                }
                Err(_e) => {
                    dprintln!("vst3: scan of {} failed: {_e}", bundle.display());
                    cache.insert(bundle, Vec::new(), true);
                    continue;
                }
            },
        };
        out.extend(classes.into_iter().map(|class| PluginCatalogEntry {
            format: PluginFormat::Vst3,
            bundle_path: bundle.clone(),
            plugin_id: class.plugin_id,
            name: class.name,
        }));
    }

    cache.save();
    out
}

/// Loads one bundle and returns its **instrument** classes, ready to cache.
///
/// This is the part that actually touches a plugin, and it only ever runs in a
/// scan child (see [`child::run_if_scan_child`](super::child::run_if_scan_child)),
/// on that child's main thread. A load failure yields an empty list — the
/// caller cannot distinguish "no instruments" from "would not load", and does
/// not need to: both mean nothing to offer from this bundle.
pub(super) fn classes_in_bundle(bundle: &Path) -> Vec<CachedClass> {
    let found = with_module(bundle, |module| classes(module.factory()));
    let found = match found {
        Ok(classes) => classes,
        Err(_e) => {
            eprintln!("vst3: {}: {_e}", bundle.display());
            return Vec::new();
        }
    };
    found
        .into_iter()
        .filter(|info| is_instrument_class(&info.category, &info.sub_categories))
        .map(|info| CachedClass {
            plugin_id: uid_to_hex(&info.cid),
            name: info.name,
        })
        .collect()
}

/// Every class a factory lists, read through the richest `getClassInfo*` call
/// that factory supports.
///
/// The three tiers are not interchangeable: only `IPluginFactory2` and up
/// report sub-categories, which is the only way to tell an instrument from an
/// effect. A factory offering the bare `IPluginFactory` is still enumerated —
/// its classes simply carry no sub-categories and are filtered out by
/// [`is_instrument_class`], which is the correct outcome (offering every audio
/// module and hoping it's an instrument would be worse than offering none).
fn classes(factory: &ComPtr<IPluginFactory>) -> Vec<ClassInfo> {
    // SAFETY: `factory` is a live factory from a loaded module. `countClasses`
    // takes no arguments and every `getClassInfo*` writes into a caller-owned
    // struct of the matching type, which is what is passed. Each index is
    // checked against the reported count.
    unsafe {
        let count = factory.countClasses();
        let factory3 = factory.cast::<IPluginFactory3>();
        let factory2 = factory.cast::<IPluginFactory2>();

        (0..count)
            .filter_map(|index| {
                if let Some(f3) = &factory3 {
                    let mut info: PClassInfoW = std::mem::zeroed();
                    if f3.getClassInfoUnicode(index, &mut info) == kResultOk {
                        return Some(ClassInfo {
                            cid: info.cid,
                            category: c_field(&info.category),
                            name: utf16_field(&info.name),
                            sub_categories: c_field(&info.subCategories),
                        });
                    }
                }
                if let Some(f2) = &factory2 {
                    let mut info: PClassInfo2 = std::mem::zeroed();
                    if f2.getClassInfo2(index, &mut info) == kResultOk {
                        return Some(ClassInfo {
                            cid: info.cid,
                            category: c_field(&info.category),
                            name: c_field(&info.name),
                            sub_categories: c_field(&info.subCategories),
                        });
                    }
                }
                let mut info: PClassInfo = std::mem::zeroed();
                if factory.getClassInfo(index, &mut info) == kResultOk {
                    return Some(ClassInfo {
                        cid: info.cid,
                        category: c_field(&info.category),
                        name: c_field(&info.name),
                        sub_categories: String::new(),
                    });
                }
                None
            })
            .collect()
    }
}

/// Whether a class is an instrument we can host: an audio module whose
/// sub-categories name `Instrument`.
fn is_instrument_class(category: &str, sub_categories: &str) -> bool {
    category == AUDIO_MODULE_CLASS
        && sub_categories
            .split('|')
            .any(|token| token.trim() == INSTRUMENT_SUBCATEGORY)
}

/// Reads a fixed-size, `NUL`-padded `char8` field out of a `PClassInfo*`.
/// VST3 pads these with zeros, but a plugin that fills the array completely
/// leaves no terminator, so the whole field is the fallback bound. Invalid
/// UTF-8 is replaced rather than dropping the class.
fn c_field(field: &[c_char]) -> String {
    let bytes: Vec<u8> = field
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Reads a fixed-size, `NUL`-padded UTF-16 field out of a [`PClassInfoW`].
/// Unpaired surrogates are replaced rather than dropping the class.
fn utf16_field(field: &[u16]) -> String {
    let units: Vec<u16> = field.iter().copied().take_while(|&c| c != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Renders a class UID as 32 lowercase hex characters, for
/// `PluginCatalogEntry::plugin_id` and the persisted `InstrumentRef`.
///
/// This is a plain hex dump of the 16 raw bytes, deliberately **not**
/// Steinberg's canonical `FUID` registry string: nothing outside Stev ever
/// reads it, so all it has to do is decode back to the same 16 bytes and stay
/// stable across runs. [`uid_from_hex`] is the matching decoder.
pub(super) fn uid_to_hex(cid: &TUID) -> String {
    cid.iter().map(|&b| format!("{:02x}", b as u8)).collect()
}

/// Parses a [`uid_to_hex`] string back into a class UID — how a persisted or
/// catalogued `plugin_id` becomes something `createInstance` can take.
///
/// `None` for anything that isn't exactly 32 hex characters: a hand-edited or
/// truncated project file leaves that track silent rather than instantiating
/// some arbitrary class that happens to parse.
pub(super) fn uid_from_hex(hex: &str) -> Option<TUID> {
    if hex.len() != 32 {
        return None;
    }
    let mut cid: TUID = [0; 16];
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    for (byte, pair) in cid.iter_mut().zip(pairs) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()? as c_char;
    }
    Some(cid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_audio_module_instruments_are_offered() {
        assert!(is_instrument_class(AUDIO_MODULE_CLASS, "Instrument|Synth"));
        assert!(is_instrument_class(AUDIO_MODULE_CLASS, "Instrument"));
        assert!(is_instrument_class(
            AUDIO_MODULE_CLASS,
            "Instrument|Synth|Sampler"
        ));
        // An effect on the same factory.
        assert!(!is_instrument_class(AUDIO_MODULE_CLASS, "Fx|Delay"));
        // The paired edit-controller class, which is not an audio module.
        assert!(!is_instrument_class("Component Controller Class", ""));
        // A bare-`IPluginFactory` class reports no sub-categories at all.
        assert!(!is_instrument_class(AUDIO_MODULE_CLASS, ""));
    }

    #[test]
    fn sub_category_matching_is_by_token_not_substring() {
        assert!(!is_instrument_class(AUDIO_MODULE_CLASS, "NotAnInstrument"));
        assert!(!is_instrument_class(AUDIO_MODULE_CLASS, "Instrumental"));
        // Whitespace around a token is tolerated.
        assert!(is_instrument_class(AUDIO_MODULE_CLASS, "Fx | Instrument "));
    }

    #[test]
    fn uid_hex_round_trips() {
        let cid: TUID = [
            0x00, 0x01, 0x7f, -0x80, -0x01, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x0a, 0x0b,
            0x0c, 0x0d,
        ];
        assert_eq!(uid_from_hex(&uid_to_hex(&cid)), Some(cid));
    }

    #[test]
    fn uid_from_hex_rejects_anything_malformed() {
        assert_eq!(uid_from_hex(""), None);
        assert_eq!(uid_from_hex("00"), None);
        // 31 and 33 characters — a truncated or padded project field.
        assert_eq!(uid_from_hex(&"a".repeat(31)), None);
        assert_eq!(uid_from_hex(&"a".repeat(33)), None);
        // Right length, not hex.
        assert_eq!(uid_from_hex(&"z".repeat(32)), None);
    }

    #[test]
    fn uid_to_hex_is_a_lowercase_dump_of_all_sixteen_bytes() {
        // Includes the sign boundary: `TUID` is `[c_char; 16]`, i.e. signed on
        // this platform, so a byte above 0x7f arrives negative and must still
        // render as its unsigned hex.
        let cid: TUID = [
            0x00, 0x01, 0x7f, -0x80, -0x01, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x0a, 0x0b,
            0x0c, 0x0d,
        ];
        let hex = uid_to_hex(&cid);
        assert_eq!(hex.len(), 32);
        assert_eq!(hex, "00017f80ff102030405060700a0b0c0d");
    }

    #[test]
    fn c_field_stops_at_the_terminator_and_tolerates_a_full_array() {
        let mut field = [0 as c_char; 8];
        for (slot, byte) in field.iter_mut().zip(b"Synth") {
            *slot = *byte as c_char;
        }
        assert_eq!(c_field(&field), "Synth");

        // No terminator anywhere: the whole array is the name.
        let full: [c_char; 4] = [
            b'A' as c_char,
            b'B' as c_char,
            b'C' as c_char,
            b'D' as c_char,
        ];
        assert_eq!(c_field(&full), "ABCD");

        assert_eq!(c_field(&[0 as c_char; 4]), "");
    }

    #[test]
    fn utf16_field_stops_at_the_terminator() {
        let mut field = [0u16; 8];
        for (slot, unit) in field.iter_mut().zip("Répro".encode_utf16()) {
            *slot = unit;
        }
        assert_eq!(utf16_field(&field), "Répro");
        assert_eq!(utf16_field(&[0u16; 4]), "");
    }
}
