//! An in-memory `IBStream` — the byte sink and source VST3 uses for plugin
//! state.
//!
//! A plugin never hands the host a buffer; it writes into a stream the host
//! provides, and reads its state back out of one. So saving a preset means
//! giving the plugin an empty [`MemoryStream`] and keeping what it wrote, and
//! restoring means handing back a stream positioned at the start.
//!
//! See `docs/180-vst3-host.md`.

use std::cell::RefCell;
use std::ffi::c_void;

use vst3::Class;
use vst3::Steinberg::IBStream_::IStreamSeekMode_::{kIBSeekCur, kIBSeekEnd, kIBSeekSet};
use vst3::Steinberg::{
    IBStream, IBStreamTrait, int32, int64, kInvalidArgument, kResultOk, tresult,
};

/// A seekable byte buffer the plugin reads and writes through.
///
/// `RefCell` rather than anything lock-free: state transfer happens on the main
/// thread during load and save, never on the audio thread.
pub(super) struct MemoryStream {
    /// The bytes, and the current position within them.
    inner: RefCell<StreamState>,
}

/// A [`MemoryStream`]'s contents and cursor.
struct StreamState {
    /// The buffer.
    bytes: Vec<u8>,
    /// Read/write position. May legitimately sit past the end after a seek;
    /// a write there zero-fills the gap, as a file would.
    pos: usize,
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl MemoryStream {
    /// An empty stream, for a plugin to write its state into.
    pub(super) fn empty() -> Self {
        Self::from_bytes(Vec::new())
    }

    /// A stream positioned at the start of `bytes`, for a plugin to read its
    /// state out of.
    pub(super) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            inner: RefCell::new(StreamState { bytes, pos: 0 }),
        }
    }

    /// Moves everything written so far out, leaving the stream empty.
    ///
    /// A `MemoryStream` lives inside a `ComWrapper`, which shares it by `Arc`,
    /// so the stream itself can't be consumed — but the buffer sits behind the
    /// `RefCell`, so it can be taken rather than copied. The caller reads it
    /// once the plugin has finished writing and drops the stream right after.
    pub(super) fn take_bytes(&self) -> Vec<u8> {
        std::mem::take(&mut self.inner.borrow_mut().bytes)
    }

    /// Rewinds to the start.
    ///
    /// Needed between handing one stream to two consumers: the first leaves the
    /// cursor at the end, and a plugin given an exhausted stream reads nothing
    /// and reports no error.
    pub(super) fn rewind(&self) {
        self.inner.borrow_mut().pos = 0;
    }
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_read: *mut int32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let mut inner = self.inner.borrow_mut();
        let available = inner.bytes.len().saturating_sub(inner.pos);
        let count = available.min(num_bytes as usize);
        // SAFETY: `buffer` is a caller-owned buffer of at least `num_bytes`
        // bytes, and `count` never exceeds that or the bytes we hold.
        unsafe {
            std::ptr::copy_nonoverlapping(
                inner.bytes[inner.pos..].as_ptr(),
                buffer.cast::<u8>(),
                count,
            );
        }
        inner.pos += count;
        if !num_bytes_read.is_null() {
            // SAFETY: caller-owned out-parameter, checked non-null.
            unsafe { *num_bytes_read = count as int32 };
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_written: *mut int32,
    ) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let count = num_bytes as usize;
        let mut inner = self.inner.borrow_mut();
        // A seek past the end followed by a write zero-fills the gap, matching
        // how a real file behaves.
        let end = inner.pos + count;
        if inner.bytes.len() < end {
            inner.bytes.resize(end, 0);
        }
        let pos = inner.pos;
        // SAFETY: `buffer` holds at least `num_bytes` readable bytes, and the
        // destination was just resized to fit exactly that many at `pos`.
        unsafe {
            std::ptr::copy_nonoverlapping(
                buffer.cast::<u8>(),
                inner.bytes[pos..].as_mut_ptr(),
                count,
            );
        }
        inner.pos = end;
        if !num_bytes_written.is_null() {
            // SAFETY: caller-owned out-parameter, checked non-null.
            unsafe { *num_bytes_written = count as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut inner = self.inner.borrow_mut();
        let base = match mode {
            m if m == kIBSeekSet as int32 => 0i64,
            m if m == kIBSeekCur as int32 => inner.pos as i64,
            m if m == kIBSeekEnd as int32 => inner.bytes.len() as i64,
            _ => return kInvalidArgument,
        };
        // Seeking before the start is an error; seeking past the end is not —
        // see `write`.
        let Some(target) = base.checked_add(pos).filter(|t| *t >= 0) else {
            return kInvalidArgument;
        };
        inner.pos = target as usize;
        if !result.is_null() {
            // SAFETY: caller-owned out-parameter, checked non-null.
            unsafe { *result = target };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: caller-owned out-parameter, checked non-null.
        unsafe { *pos = self.inner.borrow().pos as int64 };
        kResultOk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `bytes` into `stream` through the COM interface.
    fn write(stream: &MemoryStream, bytes: &[u8]) -> int32 {
        let mut written = 0;
        // SAFETY: single-threaded test; `bytes` outlives the call and `written`
        // is a valid out-parameter.
        unsafe {
            stream.write(
                bytes.as_ptr() as *mut c_void,
                bytes.len() as int32,
                &mut written,
            );
        }
        written
    }

    /// Reads up to `n` bytes out of `stream`.
    fn read(stream: &MemoryStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        // SAFETY: single-threaded test with a correctly sized buffer.
        unsafe {
            stream.read(buf.as_mut_ptr().cast(), n as int32, &mut got);
        }
        buf.truncate(got as usize);
        buf
    }

    #[test]
    fn what_a_plugin_writes_is_what_it_reads_back() {
        let stream = MemoryStream::empty();
        assert_eq!(write(&stream, b"preset-data"), 11);
        stream.rewind();
        assert_eq!(read(&stream, 32), b"preset-data");
    }

    #[test]
    fn reading_past_the_end_yields_what_is_there_and_no_more() {
        let stream = MemoryStream::from_bytes(b"abc".to_vec());
        assert_eq!(read(&stream, 10), b"abc");
        // A second read at the end returns nothing rather than repeating.
        assert!(read(&stream, 10).is_empty());
    }

    #[test]
    fn seeking_works_from_all_three_origins() {
        let stream = MemoryStream::from_bytes(b"0123456789".to_vec());
        // SAFETY: single-threaded test with valid out-parameters.
        unsafe {
            let mut at = 0i64;
            assert_eq!(stream.seek(4, kIBSeekSet as int32, &mut at), kResultOk);
            assert_eq!(at, 4);
            assert_eq!(read(&stream, 2), b"45");

            assert_eq!(stream.seek(1, kIBSeekCur as int32, &mut at), kResultOk);
            assert_eq!(at, 7);
            assert_eq!(read(&stream, 1), b"7");

            assert_eq!(stream.seek(-2, kIBSeekEnd as int32, &mut at), kResultOk);
            assert_eq!(at, 8);
            assert_eq!(read(&stream, 5), b"89");
        }
    }

    #[test]
    fn tell_reports_the_cursor() {
        let stream = MemoryStream::empty();
        write(&stream, b"abcd");
        // SAFETY: single-threaded test with a valid out-parameter.
        unsafe {
            let mut at = 0i64;
            assert_eq!(stream.tell(&mut at), kResultOk);
            assert_eq!(at, 4);
        }
        stream.rewind();
        // SAFETY: as above.
        unsafe {
            let mut at = -1i64;
            stream.tell(&mut at);
            assert_eq!(at, 0);
        }
    }

    #[test]
    fn a_write_past_the_end_zero_fills_the_gap_like_a_file() {
        let stream = MemoryStream::empty();
        // SAFETY: single-threaded test.
        unsafe {
            let mut at = 0i64;
            stream.seek(4, kIBSeekSet as int32, &mut at);
        }
        write(&stream, b"Z");
        stream.rewind();
        assert_eq!(read(&stream, 8), b"\0\0\0\0Z");
    }

    #[test]
    fn seeking_before_the_start_is_rejected() {
        let stream = MemoryStream::from_bytes(b"abc".to_vec());
        // SAFETY: single-threaded test; the invalid seek is the case under test.
        unsafe {
            let mut at = 0i64;
            assert_eq!(
                stream.seek(-1, kIBSeekSet as int32, &mut at),
                kInvalidArgument
            );
            // An unknown seek mode too.
            assert_eq!(stream.seek(0, 99, &mut at), kInvalidArgument);
        }
    }

    #[test]
    fn null_buffers_are_rejected_rather_than_dereferenced() {
        let stream = MemoryStream::empty();
        // SAFETY: passing null is exactly the case under test.
        unsafe {
            let mut n = 0i32;
            assert_eq!(
                stream.read(std::ptr::null_mut(), 4, &mut n),
                kInvalidArgument
            );
            assert_eq!(
                stream.write(std::ptr::null_mut(), 4, &mut n),
                kInvalidArgument
            );
            assert_eq!(stream.tell(std::ptr::null_mut()), kInvalidArgument);
        }
    }

    #[test]
    fn a_plugin_that_ignores_the_out_parameters_is_tolerated() {
        // Every out-parameter is optional in the API; passing null must not
        // fail the operation itself.
        let stream = MemoryStream::empty();
        // SAFETY: single-threaded test; null out-parameters are the case here.
        unsafe {
            let data = b"xy";
            assert_eq!(
                stream.write(data.as_ptr() as *mut c_void, 2, std::ptr::null_mut()),
                kResultOk
            );
            stream.rewind();
            let mut buf = [0u8; 2];
            assert_eq!(
                stream.read(buf.as_mut_ptr().cast(), 2, std::ptr::null_mut()),
                kResultOk
            );
            assert_eq!(&buf, b"xy");
        }
    }
}
