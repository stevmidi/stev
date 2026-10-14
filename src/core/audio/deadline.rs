//! Putting the [`WorkerPool`](super::WorkerPool)'s threads on the audio
//! callback's deadline.
//!
//! CoreAudio runs its IO thread (the one `cpal` calls back on) in the output
//! device's **audio workgroup**, which tells the scheduler which threads share
//! one deadline. Each worker joins that workgroup itself at start-up
//! (`os_workgroup_join` on `kAudioDevicePropertyIOThreadOSWorkgroup`, macOS
//! 11+). On Apple Silicon that is what keeps workers off the efficiency cores;
//! on Intel, where every core is alike, it measured neutral.
//!
//! Workers deliberately do **not** take a time-constraint (real-time) policy of
//! their own: under load it made Serum 2 on one track run slower and overruns
//! several times as frequent. The measurements are in
//! `docs/170-multicore-scheduling.md` § Audio workgroup.
//!
//! The join is best effort: a refused one leaves the worker as it was, and
//! [`Membership`] reports whether it took, for the overrun journal's session
//! header. Off macOS nothing here does anything, and the pool is empty there
//! anyway.

use std::marker::PhantomData;

#[cfg(target_os = "macos")]
use std::ffi::{c_int, c_void};
#[cfg(target_os = "macos")]
use std::mem::transmute;
#[cfg(target_os = "macos")]
use std::ptr::{NonNull, null, null_mut};

use cpal::Device;
#[cfg(target_os = "macos")]
use cpal::traits::DeviceTrait;
#[cfg(target_os = "macos")]
use libc::{RTLD_DEFAULT, dlsym};
#[cfg(target_os = "macos")]
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    kAudioDevicePropertyIOThreadOSWorkgroup, kAudioHardwarePropertyTranslateUIDToDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioObjectUnknown,
};
#[cfg(target_os = "macos")]
use objc2_core_foundation::CFString;

/// What every worker joins at start-up: the device IO thread's workgroup,
/// where there is one. Built once per engine and shared by the pool's workers.
pub(crate) struct DeadlineGroup {
    /// The device IO thread's workgroup, retained for the group's life.
    #[cfg(target_os = "macos")]
    workgroup: Option<Workgroup>,
}

impl DeadlineGroup {
    /// A group that changes nothing: workers stay outside any workgroup. On
    /// macOS only the tests want that.
    #[cfg_attr(all(target_os = "macos", not(test)), allow(dead_code))]
    pub(crate) fn none() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            workgroup: None,
        }
    }

    /// The workgroup of `device`'s IO thread. On macOS it is looked up through
    /// the device's UID, so it is the device the stream opened, not whatever is
    /// default now; a device or system without one (before macOS 11) gives
    /// [`none`](Self::none). Off macOS, always [`none`](Self::none).
    pub(crate) fn for_device(device: &Device) -> Self {
        #[cfg(target_os = "macos")]
        {
            let workgroup = device.id().ok().and_then(|id| device_workgroup(id.id()));
            if workgroup.is_none() {
                dprintln!("audio engine: no device workgroup; workers stay outside one");
            }
            Self { workgroup }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = device;
            Self::none()
        }
    }

    /// Joins the **calling thread** to the workgroup, if there is one. The
    /// returned [`Membership`] leaves it when dropped, which must happen on
    /// this same thread, so it is neither `Send` nor `Sync`.
    pub(crate) fn enter(&self) -> Membership<'_> {
        Membership {
            #[cfg(target_os = "macos")]
            joined: self.workgroup.as_ref().and_then(Workgroup::join),
            _bound: PhantomData,
        }
    }
}

/// One thread's place on a [`DeadlineGroup`]: whether
/// [`DeadlineGroup::enter`] joined it, and the workgroup to leave again on
/// drop.
pub(crate) struct Membership<'a> {
    /// The workgroup this thread joined and the token to leave it with.
    #[cfg(target_os = "macos")]
    joined: Option<Joined<'a>>,
    /// Ties the membership to the thread that entered (the leave must run
    /// there) and to its group.
    _bound: PhantomData<(*const (), &'a DeadlineGroup)>,
}

impl Membership<'_> {
    /// Whether the thread joined the device's audio workgroup.
    pub(crate) fn is_joined(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.joined.is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }
}

/// How many of the pool's workers joined the workgroup. Read on the audio
/// thread when an overrun is journalled, so it is `Copy` and fixed-size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkerDeadline {
    /// Worker threads in the pool.
    pub(crate) workers: u8,
    /// Of those, how many joined the device's audio workgroup.
    pub(crate) joined: u8,
}

/// `struct os_workgroup_join_token_opaque_s` on LP64 (`os/workgroup_base.h`):
/// filled by `os_workgroup_join`, handed back to `os_workgroup_leave`.
#[cfg(target_os = "macos")]
#[repr(C)]
struct JoinToken {
    /// The header's signature word.
    sig: u32,
    /// `__OS_WORKGROUP_JOIN_TOKEN_SIZE__` bytes on LP64.
    opaque: [u8; 36],
}

/// `os_workgroup_join`'s signature.
#[cfg(target_os = "macos")]
type JoinFn = unsafe extern "C" fn(*mut c_void, *mut JoinToken) -> c_int;
/// `os_workgroup_leave`'s signature.
#[cfg(target_os = "macos")]
type LeaveFn = unsafe extern "C" fn(*mut c_void, *mut JoinToken);

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// Releases an `os_object` (`os/object.h`, macOS 10.12+).
    fn os_release(object: *mut c_void);
}

/// A retained `os_workgroup_t`, released on drop.
#[cfg(target_os = "macos")]
struct Workgroup {
    /// The workgroup object, +1 retained.
    raw: NonNull<c_void>,
    /// `os_workgroup_join` and `os_workgroup_leave`, looked up at run time
    /// since they are macOS 11+ and a direct link would stop the app launching
    /// on anything older.
    join: JoinFn,
    /// See `join`.
    leave: LeaveFn,
}

// SAFETY: `os_workgroup_t` is a reference-counted `os_object`, safe to retain,
// join and release from any thread; joining and leaving are per-thread calls
// that take the object by pointer.
#[cfg(target_os = "macos")]
unsafe impl Send for Workgroup {}
#[cfg(target_os = "macos")]
unsafe impl Sync for Workgroup {}

#[cfg(target_os = "macos")]
impl Workgroup {
    /// Takes ownership of a +1 retained workgroup. `None` on a system without
    /// the join functions (before macOS 11), releasing it.
    fn adopt(raw: NonNull<c_void>) -> Option<Self> {
        // SAFETY: `dlsym` with `RTLD_DEFAULT` and NUL-terminated names; a
        // non-null result is the libSystem function of that name, whose C
        // signature `JoinFn` / `LeaveFn` mirror.
        let (join, leave) = unsafe {
            let join = dlsym(RTLD_DEFAULT, c"os_workgroup_join".as_ptr());
            let leave = dlsym(RTLD_DEFAULT, c"os_workgroup_leave".as_ptr());
            if join.is_null() || leave.is_null() {
                os_release(raw.as_ptr());
                return None;
            }
            (
                transmute::<*mut c_void, JoinFn>(join),
                transmute::<*mut c_void, LeaveFn>(leave),
            )
        };
        Some(Self { raw, join, leave })
    }

    /// Joins the calling thread. `None` if the workgroup refused — it was
    /// cancelled (the device restarted) or the thread is already in one.
    fn join(&self) -> Option<Joined<'_>> {
        // Boxed so the token keeps one address from join to leave.
        let mut token = Box::new(JoinToken {
            sig: 0,
            opaque: [0; 36],
        });
        // SAFETY: `raw` is a live workgroup and `token` a live, writable token.
        let result = unsafe { (self.join)(self.raw.as_ptr(), &raw mut *token) };
        if result != 0 {
            dprintln!("audio worker: os_workgroup_join refused ({result})");
            return None;
        }
        Some(Joined {
            workgroup: self,
            token,
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for Workgroup {
    fn drop(&mut self) {
        // SAFETY: we own the +1 retain taken when the property was read.
        unsafe { os_release(self.raw.as_ptr()) };
    }
}

/// A thread's join of a [`Workgroup`]; leaves on drop.
#[cfg(target_os = "macos")]
struct Joined<'a> {
    /// The workgroup joined.
    workgroup: &'a Workgroup,
    /// The token `os_workgroup_join` filled in.
    token: Box<JoinToken>,
}

#[cfg(target_os = "macos")]
impl Drop for Joined<'_> {
    fn drop(&mut self) {
        // SAFETY: the same workgroup and token the join succeeded with, on the
        // joining thread (`Membership` is neither `Send` nor `Sync`).
        unsafe { (self.workgroup.leave)(self.workgroup.raw.as_ptr(), &raw mut *self.token) };
    }
}

/// The IO-thread workgroup of the device whose CoreAudio UID is `uid`.
#[cfg(target_os = "macos")]
fn device_workgroup(uid: &str) -> Option<Workgroup> {
    let device = device_for_uid(uid)?;
    let address = global_address(kAudioDevicePropertyIOThreadOSWorkgroup);
    let mut workgroup: *mut c_void = null_mut();
    let mut size = size_of::<*mut c_void>() as u32;
    // SAFETY: the property is documented to write one `os_workgroup_t`, +1
    // retained (the caller releases it), into a pointer-sized buffer; `size`
    // describes `workgroup`. Older systems return an error, checked below.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut workgroup).cast(),
        )
    };
    if status != 0 {
        return None;
    }
    Workgroup::adopt(NonNull::new(workgroup)?)
}

/// The `AudioObjectID` of the device whose UID is `uid`, via
/// `kAudioHardwarePropertyTranslateUIDToDevice`.
#[cfg(target_os = "macos")]
fn device_for_uid(uid: &str) -> Option<AudioObjectID> {
    let uid = CFString::from_str(uid);
    let qualifier: *const CFString = &*uid;
    let address = global_address(kAudioHardwarePropertyTranslateUIDToDevice);
    let mut device: AudioObjectID = kAudioObjectUnknown;
    let mut size = size_of::<AudioObjectID>() as u32;
    // SAFETY: the qualifier is one `CFStringRef`, kept alive by `uid` for the
    // call, and the property writes one `AudioObjectID` into `device`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            size_of::<*const CFString>() as u32,
            (&raw const qualifier).cast(),
            NonNull::from(&mut size),
            NonNull::from(&mut device).cast(),
        )
    };
    (status == 0 && device != kAudioObjectUnknown).then_some(device)
}

/// A global-scope, main-element address for `selector`.
#[cfg(target_os = "macos")]
fn global_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_none_group_leaves_a_thread_outside_any_workgroup() {
        let group = DeadlineGroup::none();
        assert!(!group.enter().is_joined());
    }
}
