//! Minimal, dependency-free WAV reading and writing.
//!
//! Supports:
//! * RIFF/WAVE, RF64 and BW64 containers (files larger than 4 GB)
//! * Integer PCM (8/16/24/32 bit) and IEEE float (32/64 bit)
//! * `WAVE_FORMAT_EXTENSIBLE` headers
//!
//! Splitting never converts samples: the raw bytes of each selected channel are
//! copied verbatim, so the output is bit-identical to the source channel.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const WAVE_FORMAT_PCM: u16 = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// The tail of the KSDATAFORMAT_SUBTYPE_* GUIDs (after the 2-byte format code).
const SUBTYPE_GUID_TAIL: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Int,
    Float,
}

/// Description of the audio stream inside a WAV file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavFormat {
    pub channels: u16,
    pub sample_rate: u32,
    /// Bits per sample of the storage container (e.g. 32 for 24-in-32).
    pub container_bits: u16,
    /// Bits actually used (e.g. 24 for 24-in-32). Equal to `container_bits` normally.
    pub valid_bits: u16,
    pub kind: SampleKind,
}

impl WavFormat {
    pub fn bytes_per_sample(&self) -> usize {
        (self.container_bits as usize).div_ceil(8)
    }

    pub fn block_align(&self) -> usize {
        self.bytes_per_sample() * self.channels as usize
    }

    /// Human readable description like "24-bit PCM" or "32-bit float".
    pub fn describe_encoding(&self) -> String {
        match self.kind {
            SampleKind::Int => format!("{}-bit PCM", self.valid_bits),
            SampleKind::Float => format!("{}-bit float", self.valid_bits),
        }
    }
}

/// A parsed input WAV file: its format plus where the sample data lives.
#[derive(Debug, Clone)]
pub struct WavInfo {
    pub format: WavFormat,
    pub data_offset: u64,
    pub data_len: u64,
    pub file_len: u64,
}

impl WavInfo {
    pub fn frames(&self) -> u64 {
        self.data_len / self.format.block_align() as u64
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.format.sample_rate.max(1) as f64
    }
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(a)
}

fn parse_fmt(body: &[u8]) -> Result<WavFormat> {
    if body.len() < 16 {
        bail!("The 'fmt ' chunk is too short ({} bytes).", body.len());
    }
    let mut tag = u16_at(body, 0);
    let channels = u16_at(body, 2);
    let sample_rate = u32_at(body, 4);
    let block_align = u16_at(body, 12);
    let bits = u16_at(body, 14);
    let mut valid_bits = bits;

    if tag == WAVE_FORMAT_EXTENSIBLE {
        if body.len() < 40 {
            bail!("WAVE_FORMAT_EXTENSIBLE header is truncated.");
        }
        let vb = u16_at(body, 18);
        if vb != 0 && vb <= bits {
            valid_bits = vb;
        }
        tag = u16_at(body, 24); // first two bytes of the SubFormat GUID
    }

    let kind = match tag {
        WAVE_FORMAT_PCM => SampleKind::Int,
        WAVE_FORMAT_IEEE_FLOAT => SampleKind::Float,
        other => bail!(
            "Unsupported WAV encoding (format code 0x{other:04X}). \
             Only uncompressed PCM and IEEE float are supported."
        ),
    };
    if channels == 0 {
        bail!("The file reports zero channels.");
    }
    if bits == 0 || bits > 64 {
        bail!("Unsupported bit depth: {bits}.");
    }

    // Some writers store a container size in block_align that is larger than
    // `bits` suggests (e.g. 24-bit samples in 4-byte containers with bits=24).
    // Trust block_align when it is consistent with the channel count.
    let mut container_bits = bits.div_ceil(8) * 8;
    if block_align != 0 && block_align.is_multiple_of(channels) {
        let per_sample = block_align / channels;
        if per_sample * 8 >= bits && per_sample <= 8 {
            container_bits = per_sample * 8;
        }
    }

    Ok(WavFormat {
        channels,
        sample_rate,
        container_bits,
        valid_bits: valid_bits.min(container_bits),
        kind,
    })
}

/// Parse the header of a WAV / RF64 / BW64 file.
pub fn read_info(path: &Path) -> Result<WavInfo> {
    let mut f = File::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    let file_len = f.metadata()?.len();

    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr)
        .context("The file is too short to be a WAV file.")?;
    let is_rf64 = match &hdr[0..4] {
        b"RIFF" => false,
        b"RF64" | b"BW64" => true,
        b"RIFX" => bail!("Big-endian (RIFX) WAV files are not supported."),
        _ => bail!("This is not a WAV file (missing RIFF header)."),
    };
    if &hdr[8..12] != b"WAVE" {
        bail!("This is not a WAV file (missing WAVE identifier).");
    }

    let mut pos = 12u64;
    let mut ds64_data_len: Option<u64> = None;
    let mut format: Option<WavFormat> = None;
    let mut data: Option<(u64, u64)> = None;

    while pos + 8 <= file_len {
        f.seek(SeekFrom::Start(pos))?;
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch)?;
        let id = [ch[0], ch[1], ch[2], ch[3]];
        let size32 = u32_at(&ch, 4);
        let body = pos + 8;
        let mut size = size32 as u64;

        match &id {
            b"ds64" => {
                let mut b = vec![0u8; (size32 as usize).min(64)];
                f.read_exact(&mut b).context("Truncated ds64 chunk.")?;
                if b.len() >= 16 {
                    ds64_data_len = Some(u64_at(&b, 8));
                }
            }
            b"fmt " => {
                if size32 > 4096 {
                    bail!("The 'fmt ' chunk is implausibly large.");
                }
                let mut b = vec![0u8; size32 as usize];
                f.read_exact(&mut b).context("Truncated 'fmt ' chunk.")?;
                format = Some(parse_fmt(&b)?);
            }
            b"data" => {
                if is_rf64 && size32 == 0xFFFF_FFFF {
                    size = ds64_data_len.unwrap_or(u64::MAX);
                }
                let available = file_len.saturating_sub(body);
                // Recordings that were interrupted often leave the size as 0 or
                // larger than the file; fall back to "everything that is there".
                if size == 0 || size > available {
                    size = available;
                }
                data = Some((body, size));
                if format.is_some() {
                    break;
                }
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }

    let format = format.context("The WAV file has no 'fmt ' chunk.")?;
    let (data_offset, data_len) = data.context("The WAV file has no 'data' chunk.")?;
    let block = format.block_align() as u64;
    Ok(WavInfo {
        data_len: data_len - data_len % block,
        format,
        data_offset,
        file_len,
    })
}

/// Streaming WAV writer. Starts as plain RIFF and is upgraded to RF64 on
/// `finalize` if the data turned out to be larger than 4 GB.
pub struct WavWriter {
    w: BufWriter<File>,
    format: WavFormat,
    data_size_pos: u64,
    data_start: u64,
    data_bytes: u64,
    /// Largest RIFF size before switching to RF64 (lowered in tests).
    rf64_threshold: u64,
}

impl WavWriter {
    pub fn create(path: &Path, format: WavFormat) -> Result<Self> {
        let file =
            File::create(path).with_context(|| format!("Could not create {}", path.display()))?;
        let mut w = BufWriter::with_capacity(1 << 20, file);

        // Plain format tags are the most widely compatible. EXTENSIBLE is only
        // needed when the valid bits differ from the container size.
        let extensible = format.valid_bits != format.container_bits;
        let fmt_len: u32 = if extensible { 40 } else { 16 };
        let tag = match format.kind {
            SampleKind::Int => WAVE_FORMAT_PCM,
            SampleKind::Float => WAVE_FORMAT_IEEE_FLOAT,
        };
        let block_align = format.block_align() as u16;

        let mut h: Vec<u8> = Vec::with_capacity(100);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&0u32.to_le_bytes()); // patched in finalize
        h.extend_from_slice(b"WAVE");
        // Placeholder that becomes the ds64 chunk if the file exceeds 4 GB.
        h.extend_from_slice(b"JUNK");
        h.extend_from_slice(&28u32.to_le_bytes());
        h.extend_from_slice(&[0u8; 28]);
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&fmt_len.to_le_bytes());
        h.extend_from_slice(
            &(if extensible {
                WAVE_FORMAT_EXTENSIBLE
            } else {
                tag
            })
            .to_le_bytes(),
        );
        h.extend_from_slice(&format.channels.to_le_bytes());
        h.extend_from_slice(&format.sample_rate.to_le_bytes());
        h.extend_from_slice(&(format.sample_rate * block_align as u32).to_le_bytes());
        h.extend_from_slice(&block_align.to_le_bytes());
        h.extend_from_slice(&format.container_bits.to_le_bytes());
        if extensible {
            let mask: u32 = match format.channels {
                1 => 0x4, // front center
                2 => 0x3, // front left | front right
                _ => 0,
            };
            h.extend_from_slice(&22u16.to_le_bytes());
            h.extend_from_slice(&format.valid_bits.to_le_bytes());
            h.extend_from_slice(&mask.to_le_bytes());
            h.extend_from_slice(&tag.to_le_bytes());
            h.extend_from_slice(&SUBTYPE_GUID_TAIL);
        }
        h.extend_from_slice(b"data");
        let data_size_pos = h.len() as u64;
        h.extend_from_slice(&0u32.to_le_bytes()); // patched in finalize
        let data_start = h.len() as u64;
        w.write_all(&h)?;

        Ok(Self {
            w,
            format,
            data_size_pos,
            data_start,
            data_bytes: 0,
            rf64_threshold: u32::MAX as u64,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.w.write_all(bytes)?;
        self.data_bytes += bytes.len() as u64;
        Ok(())
    }

    pub fn finalize(mut self) -> Result<()> {
        let pad = self.data_bytes & 1;
        if pad == 1 {
            self.w.write_all(&[0])?;
        }
        let riff_size = self.data_start + self.data_bytes + pad - 8;
        let frames = self.data_bytes / self.format.block_align() as u64;

        if riff_size <= self.rf64_threshold {
            self.w.seek(SeekFrom::Start(4))?;
            self.w.write_all(&(riff_size as u32).to_le_bytes())?;
            self.w.seek(SeekFrom::Start(self.data_size_pos))?;
            self.w.write_all(&(self.data_bytes as u32).to_le_bytes())?;
        } else {
            self.w.seek(SeekFrom::Start(0))?;
            self.w.write_all(b"RF64")?;
            self.w.write_all(&u32::MAX.to_le_bytes())?;
            self.w.seek(SeekFrom::Start(12))?;
            self.w.write_all(b"ds64")?;
            self.w.write_all(&28u32.to_le_bytes())?;
            self.w.write_all(&riff_size.to_le_bytes())?;
            self.w.write_all(&self.data_bytes.to_le_bytes())?;
            self.w.write_all(&frames.to_le_bytes())?;
            self.w.write_all(&0u32.to_le_bytes())?; // table length
            self.w.seek(SeekFrom::Start(self.data_size_pos))?;
            self.w.write_all(&u32::MAX.to_le_bytes())?;
        }
        self.w.flush()?;
        Ok(())
    }
}

impl WavWriter {
    /// Always write RF64 regardless of size (for testing interoperability).
    #[doc(hidden)]
    pub fn force_rf64(&mut self) {
        self.rf64_threshold = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mws-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn fmt(ch: u16, bits: u16, valid: u16, kind: SampleKind) -> WavFormat {
        WavFormat {
            channels: ch,
            sample_rate: 48000,
            container_bits: bits,
            valid_bits: valid,
            kind,
        }
    }

    fn roundtrip(f: WavFormat, frames: usize, rf64: bool) {
        let p = tmp(&format!(
            "rt-{}-{}-{}-{rf64}.wav",
            f.channels, f.container_bits, f.valid_bits
        ));
        let data: Vec<u8> = (0..frames * f.block_align())
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        let mut w = WavWriter::create(&p, f.clone()).unwrap();
        if rf64 {
            w.force_rf64();
        }
        w.write(&data).unwrap();
        w.finalize().unwrap();

        let info = read_info(&p).unwrap();
        assert_eq!(info.format, f);
        assert_eq!(info.frames(), frames as u64);
        let bytes = std::fs::read(&p).unwrap();
        let start = info.data_offset as usize;
        assert_eq!(&bytes[start..start + data.len()], &data[..]);
        assert_eq!(&bytes[0..4], if rf64 { b"RF64" } else { b"RIFF" });
        // RIFF size must match the file (odd data gets a pad byte).
        if !rf64 {
            assert_eq!(u32_at(&bytes, 4) as usize + 8, bytes.len());
        }
    }

    #[test]
    fn roundtrip_formats() {
        roundtrip(fmt(1, 16, 16, SampleKind::Int), 1001, false);
        roundtrip(fmt(2, 24, 24, SampleKind::Int), 999, false);
        roundtrip(fmt(1, 24, 24, SampleKind::Int), 333, false); // odd data length
        roundtrip(fmt(2, 32, 24, SampleKind::Int), 500, false); // extensible 24-in-32
        roundtrip(fmt(2, 32, 32, SampleKind::Float), 500, false);
        roundtrip(fmt(1, 64, 64, SampleKind::Float), 500, false);
        roundtrip(fmt(1, 8, 8, SampleKind::Int), 777, false);
        roundtrip(fmt(2, 24, 24, SampleKind::Int), 999, true);
        roundtrip(fmt(1, 24, 24, SampleKind::Int), 333, true);
    }

    #[test]
    fn rejects_non_wav() {
        let p = tmp("not.wav");
        std::fs::write(&p, b"hello world, this is not a wav file").unwrap();
        assert!(read_info(&p).is_err());
    }

    #[test]
    fn truncated_recording_uses_available_data() {
        let f = fmt(4, 24, 24, SampleKind::Int);
        let p = tmp("trunc.wav");
        let mut w = WavWriter::create(&p, f.clone()).unwrap();
        w.write(&vec![1u8; 100 * f.block_align()]).unwrap();
        w.finalize().unwrap();
        // Simulate a crashed recorder: data size left at 0, plus half a frame of junk.
        let mut bytes = std::fs::read(&p).unwrap();
        let info = read_info(&p).unwrap();
        let pos = info.data_offset as usize - 4;
        bytes[pos..pos + 4].copy_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[9u8; 5]);
        std::fs::write(&p, &bytes).unwrap();
        let info = read_info(&p).unwrap();
        assert_eq!(info.frames(), 100);
    }
}
