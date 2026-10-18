//! A streaming WAV writer for the audio export (`300-export-to-audio.md`),
//! hand-rolled like the MIDI clip export's `smf.rs` (no dependency): stereo 32-bit float
//! (`WAVE_FORMAT_IEEE_FLOAT`) at the device's sample rate, so no level is
//! clipped or rounded before the DAW sees it.
//!
//! Frames are written as they arrive; [`WavWriter::finish`] seeks back and
//! patches the three sizes the header can't know up front (`RIFF`, `fact`,
//! `data`). A file that would pass the format's 4 GiB limit refuses the
//! frames that would cross it.

use std::io::{self, ErrorKind, Seek, SeekFrom, Write};

/// `WAVE_FORMAT_IEEE_FLOAT`: 32-bit float samples.
const FORMAT_IEEE_FLOAT: u16 = 3;

/// Interleaved channels per frame: the export is always stereo.
const CHANNELS: u16 = 2;

/// Bits per sample.
const BITS_PER_SAMPLE: u16 = 32;

/// Bytes per interleaved frame.
const BYTES_PER_FRAME: u32 = CHANNELS as u32 * BITS_PER_SAMPLE as u32 / 8;

/// Length of everything before the sample data: `RIFF` + size + `WAVE`
/// (12), the 18-byte `fmt ` chunk a non-PCM format needs (26), the `fact`
/// chunk (12), and the `data` chunk header (8).
pub(crate) const HEADER_LEN: u32 = 58;

/// Where the `RIFF` size sits, after the `RIFF` tag.
const RIFF_SIZE_OFFSET: u64 = 4;

/// Where the `data` chunk's size sits: the header's last field.
const DATA_SIZE_OFFSET: u64 = HEADER_LEN as u64 - 4;

/// Where the `fact` chunk's frame count sits: just before the `data` chunk
/// header.
const FACT_FRAMES_OFFSET: u64 = DATA_SIZE_OFFSET - 8;

/// The most sample bytes a file can hold: the `RIFF` size (everything after
/// its own 8 bytes) is a `u32`, rounded down to whole frames.
const MAX_DATA_BYTES: u32 = (u32::MAX - (HEADER_LEN - 8)) / BYTES_PER_FRAME * BYTES_PER_FRAME;

/// A stereo float WAV being written to `W`, one block of frames at a time.
pub(crate) struct WavWriter<W: Write + Seek> {
    /// Where the file goes, positioned after the last frame written.
    out: W,
    /// Sample bytes written so far.
    data_bytes: u32,
    /// The little-endian bytes of the frames being written, kept between
    /// calls so a write doesn't allocate.
    bytes: Vec<u8>,
}

impl<W: Write + Seek> WavWriter<W> {
    /// Starts a file at `sample_rate` frames per second (rounded to whole
    /// Hz), writing its header with the sizes left at zero for
    /// [`finish`](Self::finish) to patch.
    pub(crate) fn new(mut out: W, sample_rate: f64) -> io::Result<Self> {
        let rate = sample_rate.round() as u32;
        let mut header = Vec::with_capacity(HEADER_LEN as usize);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(HEADER_LEN - 8).to_le_bytes());
        header.extend_from_slice(b"WAVE");
        header.extend_from_slice(b"fmt ");
        header.extend_from_slice(&18u32.to_le_bytes());
        header.extend_from_slice(&FORMAT_IEEE_FLOAT.to_le_bytes());
        header.extend_from_slice(&CHANNELS.to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&(rate * BYTES_PER_FRAME).to_le_bytes());
        header.extend_from_slice(&(BYTES_PER_FRAME as u16).to_le_bytes());
        header.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
        // `cbSize`: no extension bytes follow.
        header.extend_from_slice(&0u16.to_le_bytes());
        header.extend_from_slice(b"fact");
        header.extend_from_slice(&4u32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&0u32.to_le_bytes());
        debug_assert_eq!(header.len(), HEADER_LEN as usize);
        out.write_all(&header)?;
        Ok(Self {
            out,
            data_bytes: 0,
            bytes: Vec::new(),
        })
    }

    /// Appends interleaved `[left, right, left, right, …]` samples, a whole
    /// number of frames. Refused whole, with nothing written, when they would
    /// take the file past the format's 4 GiB limit.
    pub(crate) fn write_interleaved(&mut self, samples: &[f32]) -> io::Result<()> {
        debug_assert!(samples.len().is_multiple_of(CHANNELS as usize));
        let bytes = u32::try_from(size_of_val(samples))
            .ok()
            .filter(|bytes| *bytes <= MAX_DATA_BYTES - self.data_bytes)
            .ok_or_else(|| {
                io::Error::new(ErrorKind::FileTooLarge, "past the WAV format's 4 GiB")
            })?;
        self.bytes.clear();
        self.bytes
            .extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        self.out.write_all(&self.bytes)?;
        self.data_bytes += bytes;
        Ok(())
    }

    /// Frames written so far.
    pub(crate) fn frames(&self) -> u32 {
        self.data_bytes / BYTES_PER_FRAME
    }

    /// Patches the header's sizes and flushes; hands back the writer,
    /// positioned at the file's end.
    pub(crate) fn finish(mut self) -> io::Result<W> {
        let frames = self.frames();
        self.patch(RIFF_SIZE_OFFSET, HEADER_LEN - 8 + self.data_bytes)?;
        self.patch(FACT_FRAMES_OFFSET, frames)?;
        self.patch(DATA_SIZE_OFFSET, self.data_bytes)?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// Overwrites the `u32` at byte `offset`.
    fn patch(&mut self, offset: u64, value: u32) -> io::Result<()> {
        self.out.seek(SeekFrom::Start(offset))?;
        self.out.write_all(&value.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// The sample rate and interleaved samples of a file this writer made —
    /// a reader for exactly its layout, nothing more.
    fn read_back(bytes: &[u8]) -> (u32, Vec<f32>) {
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let data_len = u32_at(bytes, DATA_SIZE_OFFSET as usize) as usize;
        let data = &bytes[HEADER_LEN as usize..HEADER_LEN as usize + data_len];
        let samples = data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        (u32_at(bytes, 24), samples)
    }

    fn written(sample_rate: f64, blocks: &[&[f32]]) -> Vec<u8> {
        let mut wav = WavWriter::new(Cursor::new(Vec::new()), sample_rate).unwrap();
        for block in blocks {
            wav.write_interleaved(block).unwrap();
        }
        wav.finish().unwrap().into_inner()
    }

    #[test]
    fn the_header_is_stereo_float_at_the_sample_rate() {
        let bytes = written(48_000.0, &[&[0.5, -0.5]]);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(&bytes, 16), 18, "fmt chunk size");
        assert_eq!(u16_at(&bytes, 20), FORMAT_IEEE_FLOAT);
        assert_eq!(u16_at(&bytes, 22), 2, "channels");
        assert_eq!(u32_at(&bytes, 24), 48_000, "sample rate");
        assert_eq!(u32_at(&bytes, 28), 48_000 * 8, "byte rate");
        assert_eq!(u16_at(&bytes, 32), 8, "block align");
        assert_eq!(u16_at(&bytes, 34), 32, "bits per sample");
        assert_eq!(u16_at(&bytes, 36), 0, "cbSize");
        assert_eq!(&bytes[38..42], b"fact");
        assert_eq!(u32_at(&bytes, 42), 4, "fact chunk size");
        assert_eq!(&bytes[50..54], b"data");
    }

    #[test]
    fn finish_patches_every_size() {
        // Three frames, over two blocks.
        let bytes = written(44_100.0, &[&[0.1, 0.2, 0.3, 0.4], &[0.5, 0.6]]);
        assert_eq!(bytes.len(), HEADER_LEN as usize + 24);
        assert_eq!(u32_at(&bytes, 4), bytes.len() as u32 - 8, "RIFF size");
        assert_eq!(
            u32_at(&bytes, FACT_FRAMES_OFFSET as usize),
            3,
            "fact frames"
        );
        assert_eq!(u32_at(&bytes, DATA_SIZE_OFFSET as usize), 24, "data size");
    }

    #[test]
    fn an_empty_file_is_still_a_valid_header() {
        let bytes = written(48_000.0, &[]);
        assert_eq!(bytes.len(), HEADER_LEN as usize);
        assert_eq!(u32_at(&bytes, 4), HEADER_LEN - 8);
        assert_eq!(u32_at(&bytes, DATA_SIZE_OFFSET as usize), 0);
    }

    #[test]
    fn samples_round_trip_exactly() {
        // Float, so levels past full scale and tiny ones come back untouched.
        let samples = [0.0, -0.0, 1.5, -2.25, 1e-30, f32::MIN_POSITIVE];
        let (rate, back) = read_back(&written(96_000.0, &[&samples]));
        assert_eq!(rate, 96_000);
        assert_eq!(back, samples);
    }

    #[test]
    fn a_fractional_device_rate_rounds_to_whole_hz() {
        let (rate, _) = read_back(&written(44_099.6, &[]));
        assert_eq!(rate, 44_100);
    }

    #[test]
    fn frames_past_the_4_gib_limit_are_refused_whole() {
        let mut wav = WavWriter::new(Cursor::new(Vec::new()), 48_000.0).unwrap();
        // Pretend the file is one frame short of full.
        wav.data_bytes = MAX_DATA_BYTES - BYTES_PER_FRAME;
        let refused = wav.write_interleaved(&[0.0; 4]).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::FileTooLarge);
        assert_eq!(wav.data_bytes, MAX_DATA_BYTES - BYTES_PER_FRAME);
        // The last frame still fits.
        wav.write_interleaved(&[0.0; 2]).unwrap();
        assert_eq!(wav.data_bytes, MAX_DATA_BYTES);
    }
}
