//! Host-created `IMessage` / `IAttributeList` objects — how the two halves of a
//! **dual-component** plugin talk to each other.
//!
//! A VST3 plugin may be one object implementing both `IComponent` and
//! `IEditController`, or two separate objects. In the split case the processor
//! and the UI share no state at all, and everything that has to cross between
//! them goes through a connection point as an `IMessage` — which, crucially,
//! **the plugin cannot allocate itself**. It asks the host, through
//! `IHostApplication::createInstance`, and a host that declines leaves the two
//! halves unable to talk.
//!
//! The symptom when this is missing is precise and misleading: the plugin
//! loads, the editor opens, its preset browser works and every parameter in the
//! UI updates — and the sound never changes, because only the controller ever
//! heard about any of it. Spire is one such plugin.
//!
//! See `docs/180-vst3-host.md`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};

use vst3::Steinberg::Vst::IAttributeList_::AttrID;
use vst3::Steinberg::Vst::{IAttributeList, IAttributeListTrait, IMessage, IMessageTrait, TChar};
use vst3::Steinberg::{
    FIDString, int64, kInvalidArgument, kResultFalse, kResultOk, tresult, uint32,
};
use vst3::{Class, ComWrapper};

use super::host::write_utf16_terminated;

/// One attribute's value. VST3 attributes are dynamically typed and a reader
/// must get back what the writer put in — asking for an int that was stored as
/// a float is an error, not a conversion.
enum AttrValue {
    /// `setInt` / `getInt`.
    Int(i64),
    /// `setFloat` / `getFloat`.
    Float(f64),
    /// `setString` / `getString`, stored as the UTF-16 the API uses.
    Text(Vec<u16>),
    /// `setBinary` / `getBinary`. The host owns this buffer; `getBinary` hands
    /// back a pointer into it, valid while the attribute list lives.
    Binary(Vec<u8>),
}

/// The key/value bag carried by an [`HostMessage`].
///
/// Keys are borrowed `const char*` from the plugin, so they are copied into
/// owned `CString`s — the plugin makes no promise about how long its key
/// pointer stays valid, and these outlive the call.
pub(super) struct HostAttributeList {
    /// The attributes, by key. `RefCell` because the interface is `&self` but
    /// this is genuinely mutable; VST3 scopes attribute lists to the UI/message
    /// thread, so there is no cross-thread access to guard.
    attrs: RefCell<HashMap<CString, AttrValue>>,
}

impl Class for HostAttributeList {
    type Interfaces = (IAttributeList,);
}

impl HostAttributeList {
    /// An empty attribute list.
    pub(super) fn new() -> Self {
        Self {
            attrs: RefCell::new(HashMap::new()),
        }
    }
}

/// Borrows a plugin-supplied attribute key, `None` if it is null. Lookups use
/// it as is (a `HashMap<CString, _>` is queried by `&CStr`); only a store
/// copies it.
///
/// # Safety
///
/// `id` must be null or a valid `NUL`-terminated C string, as the API
/// requires, that outlives the returned borrow.
unsafe fn key<'a>(id: AttrID) -> Option<&'a CStr> {
    if id.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a valid C string for the borrow's life.
    Some(unsafe { CStr::from_ptr(id) })
}

impl HostAttributeList {
    /// Stores `value` under the plugin's key `id`, copying the key.
    ///
    /// # Safety
    ///
    /// As for [`key`].
    unsafe fn store(&self, id: AttrID, value: AttrValue) -> tresult {
        // SAFETY: forwarded from the caller.
        let Some(key) = (unsafe { key(id) }) else {
            return kInvalidArgument;
        };
        self.attrs.borrow_mut().insert(key.to_owned(), value);
        kResultOk
    }
}

impl IAttributeListTrait for HostAttributeList {
    unsafe fn setInt(&self, id: AttrID, value: int64) -> tresult {
        // SAFETY: `id` is the API's `NUL`-terminated key.
        unsafe { self.store(id, AttrValue::Int(value)) }
    }

    unsafe fn getInt(&self, id: AttrID, value: *mut int64) -> tresult {
        if value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `id` is the API's key; `value` is a caller-owned `int64`.
        unsafe {
            let Some(key) = key(id) else {
                return kInvalidArgument;
            };
            match self.attrs.borrow().get(key) {
                Some(AttrValue::Int(v)) => {
                    *value = *v;
                    kResultOk
                }
                _ => kResultFalse,
            }
        }
    }

    unsafe fn setFloat(&self, id: AttrID, value: f64) -> tresult {
        // SAFETY: `id` is the API's key.
        unsafe { self.store(id, AttrValue::Float(value)) }
    }

    unsafe fn getFloat(&self, id: AttrID, value: *mut f64) -> tresult {
        if value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `id` is the API's key; `value` is a caller-owned `f64`.
        unsafe {
            let Some(key) = key(id) else {
                return kInvalidArgument;
            };
            match self.attrs.borrow().get(key) {
                Some(AttrValue::Float(v)) => {
                    *value = *v;
                    kResultOk
                }
                _ => kResultFalse,
            }
        }
    }

    unsafe fn setString(&self, id: AttrID, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `id` is the API's key; `string` is a `NUL`-terminated UTF-16
        // buffer owned by the caller, copied here.
        unsafe {
            let mut text = Vec::new();
            let mut p = string;
            while *p != 0 {
                text.push(*p);
                p = p.add(1);
            }
            self.store(id, AttrValue::Text(text))
        }
    }

    unsafe fn getString(&self, id: AttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        // `sizeInBytes`, not units — the buffer holds half as many `TChar`s.
        let capacity = (size_in_bytes as usize) / size_of::<TChar>();
        if capacity == 0 {
            return kInvalidArgument;
        }
        // SAFETY: `id` is the API's key; `string` is a caller-owned buffer of
        // `capacity` UTF-16 units, and the write below stays inside it and
        // always terminates.
        unsafe {
            let Some(key) = key(id) else {
                return kInvalidArgument;
            };
            let attrs = self.attrs.borrow();
            let Some(AttrValue::Text(text)) = attrs.get(key) else {
                return kResultFalse;
            };
            let out = std::slice::from_raw_parts_mut(string, capacity);
            write_utf16_terminated(out, text.iter().copied());
        }
        kResultOk
    }

    unsafe fn setBinary(&self, id: AttrID, data: *const c_void, size_in_bytes: uint32) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `id` is the API's key; `data` points at `size_in_bytes` bytes
        // owned by the caller, copied here because the plugin makes no promise
        // about how long they stay valid.
        unsafe {
            let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size_in_bytes as usize);
            self.store(id, AttrValue::Binary(bytes.to_vec()))
        }
    }

    unsafe fn getBinary(
        &self,
        id: AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        if data.is_null() || size_in_bytes.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: `id` is the API's key; both out-parameters are caller-owned.
        // The pointer handed back borrows the stored `Vec`, which lives as long
        // as this attribute list — the ownership `getBinary` implies.
        unsafe {
            let Some(key) = key(id) else {
                return kInvalidArgument;
            };
            let attrs = self.attrs.borrow();
            let Some(AttrValue::Binary(bytes)) = attrs.get(key) else {
                return kResultFalse;
            };
            *data = bytes.as_ptr().cast();
            *size_in_bytes = bytes.len() as uint32;
        }
        kResultOk
    }
}

/// A message passed between a plugin's component and controller.
pub(super) struct HostMessage {
    /// The message id the sender set.
    id: RefCell<CString>,
    /// The payload. Held as a `ComWrapper` because `getAttributes` hands the
    /// plugin a borrowed pointer that must stay valid for the message's life.
    attributes: ComWrapper<HostAttributeList>,
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl HostMessage {
    /// An empty message with no id.
    pub(super) fn new() -> Self {
        Self {
            id: RefCell::new(CString::default()),
            attributes: ComWrapper::new(HostAttributeList::new()),
        }
    }
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        // Borrowing out of the `RefCell` is sound here because the pointer is
        // read by the caller before anything can call `setMessageID` again:
        // messages are used on one thread, and `getMessageID` is documented as
        // returning a borrow owned by the message.
        self.id.borrow().as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        // SAFETY: `id` is the API's `NUL`-terminated string, copied here.
        if let Some(id) = unsafe { key(id) }.map(CStr::to_owned) {
            *self.id.borrow_mut() = id;
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map(|r| r.as_ptr())
            .unwrap_or(std::ptr::null_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::Steinberg::Vst::IAttributeList_iid;
    use vst3::Steinberg::Vst::IMessage_iid;

    fn k(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    #[test]
    fn ints_floats_and_binaries_round_trip() {
        let list = HostAttributeList::new();
        // SAFETY: single-threaded test with valid keys and out-parameters.
        unsafe {
            let key = k("count");
            assert_eq!(list.setInt(key.as_ptr(), 42), kResultOk);
            let mut out = 0i64;
            assert_eq!(list.getInt(key.as_ptr(), &mut out), kResultOk);
            assert_eq!(out, 42);

            let key = k("gain");
            assert_eq!(list.setFloat(key.as_ptr(), 0.25), kResultOk);
            let mut out = 0.0f64;
            assert_eq!(list.getFloat(key.as_ptr(), &mut out), kResultOk);
            assert!((out - 0.25).abs() < 1e-12);

            let key = k("blob");
            let payload = [1u8, 2, 3, 4];
            assert_eq!(
                list.setBinary(key.as_ptr(), payload.as_ptr().cast(), 4),
                kResultOk
            );
            let mut data: *const c_void = std::ptr::null();
            let mut size: uint32 = 0;
            assert_eq!(
                list.getBinary(key.as_ptr(), &mut data, &mut size),
                kResultOk
            );
            assert_eq!(size, 4);
            assert_eq!(std::slice::from_raw_parts(data.cast::<u8>(), 4), &payload);
        }
    }

    #[test]
    fn strings_round_trip_and_are_terminated_within_the_buffer() {
        let list = HostAttributeList::new();
        let key = k("name");
        let text: Vec<u16> = "Preset\0".encode_utf16().collect();
        // SAFETY: single-threaded test; `text` is NUL-terminated UTF-16 and the
        // output buffer is sized in bytes as the API specifies.
        unsafe {
            assert_eq!(list.setString(key.as_ptr(), text.as_ptr()), kResultOk);
            let mut out = [0xFFFFu16; 16];
            let bytes = (out.len() * size_of::<TChar>()) as uint32;
            assert_eq!(
                list.getString(key.as_ptr(), out.as_mut_ptr(), bytes),
                kResultOk
            );
            let s: String = String::from_utf16_lossy(
                &out.iter()
                    .copied()
                    .take_while(|&c| c != 0)
                    .collect::<Vec<_>>(),
            );
            assert_eq!(s, "Preset");
            assert_eq!(out[15], 0);
        }
    }

    #[test]
    fn an_overlong_string_is_truncated_and_still_terminated() {
        let list = HostAttributeList::new();
        let key = k("name");
        let text: Vec<u16> = "ABCDEFGHIJ\0".encode_utf16().collect();
        // SAFETY: as above.
        unsafe {
            list.setString(key.as_ptr(), text.as_ptr());
            let mut out = [0xFFFFu16; 4];
            let bytes = (out.len() * size_of::<TChar>()) as uint32;
            assert_eq!(
                list.getString(key.as_ptr(), out.as_mut_ptr(), bytes),
                kResultOk
            );
            assert_eq!(out[3], 0);
            assert_eq!(String::from_utf16_lossy(&out[..3]), "ABC");
        }
    }

    #[test]
    fn reading_an_attribute_as_the_wrong_type_fails_rather_than_converting() {
        let list = HostAttributeList::new();
        let key = k("x");
        // SAFETY: single-threaded test.
        unsafe {
            list.setInt(key.as_ptr(), 7);
            let mut f = 0.0f64;
            assert_eq!(list.getFloat(key.as_ptr(), &mut f), kResultFalse);
            let mut i = 0i64;
            assert_eq!(list.getInt(key.as_ptr(), &mut i), kResultOk);
        }
    }

    #[test]
    fn a_missing_attribute_reports_false_not_a_bogus_value() {
        let list = HostAttributeList::new();
        let key = k("absent");
        // SAFETY: single-threaded test.
        unsafe {
            let mut out = 99i64;
            assert_eq!(list.getInt(key.as_ptr(), &mut out), kResultFalse);
            assert_eq!(out, 99, "the out-parameter must be left alone");
        }
    }

    #[test]
    fn null_keys_and_out_parameters_are_rejected() {
        let list = HostAttributeList::new();
        let key = k("x");
        // SAFETY: passing nulls is exactly the case under test.
        unsafe {
            assert_eq!(list.setInt(std::ptr::null(), 1), kInvalidArgument);
            assert_eq!(
                list.getInt(key.as_ptr(), std::ptr::null_mut()),
                kInvalidArgument
            );
            assert_eq!(
                list.setBinary(key.as_ptr(), std::ptr::null(), 4),
                kInvalidArgument
            );
            assert_eq!(
                list.getString(key.as_ptr(), std::ptr::null_mut(), 16),
                kInvalidArgument
            );
        }
    }

    #[test]
    fn a_message_carries_its_id_and_a_usable_attribute_list() {
        let msg = HostMessage::new();
        let id = k("preset-changed");
        // SAFETY: single-threaded test with a valid C string.
        unsafe {
            msg.setMessageID(id.as_ptr());
            assert_eq!(CStr::from_ptr(msg.getMessageID()), id.as_c_str());
            assert!(!msg.getAttributes().is_null());
        }
    }

    #[test]
    fn the_two_host_created_class_ids_are_distinct() {
        // `createInstance` dispatches on these; conflating them would hand a
        // plugin the wrong object.
        assert_ne!(IMessage_iid, IAttributeList_iid);
    }
}
