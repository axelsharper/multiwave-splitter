//! Planning and executing a channel split.

use crate::wav::{read_info, WavFormat, WavInfo, WavWriter};
use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// One output file: which source channels (0-based) go into it, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpec {
    pub channels: Vec<u16>,
    pub path: PathBuf,
}

/// Suffix for an output, using 1-based channel numbers: "CH 3" or "CH 1-2".
pub fn channel_label(channels: &[u16]) -> String {
    match channels {
        [a] => format!("CH {}", a + 1),
        [a, b] => format!("CH {}-{}", a + 1, b + 1),
        _ => {
            let list: Vec<String> = channels.iter().map(|c| (c + 1).to_string()).collect();
            format!("CH {}", list.join("+"))
        }
    }
}

/// Default output base (folder + file stem) for an input file:
/// `/rec/Show.wav` -> `/rec/Show`.
pub fn default_base(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    input.with_file_name(stem)
}

/// Turn a user-chosen save path into a base: strips a trailing `.wav`.
pub fn base_from_user_path(p: &Path) -> PathBuf {
    match p.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("wav") => p.with_extension(""),
        _ => p.to_path_buf(),
    }
}

/// `<base> CH 1-2.wav`
pub fn output_path(base: &Path, channels: &[u16]) -> PathBuf {
    let stem = base
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    base.with_file_name(format!("{stem} {}.wav", channel_label(channels)))
}

/// Default Stereo Link state: channels 1-2, 3-4, ... linked.
pub fn default_links(channels: u16) -> Vec<bool> {
    (0..channels)
        .map(|c| c % 2 == 0 && c + 1 < channels)
        .collect()
}

/// A row group in the channel list: a single channel or a stereo-linked pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkGroup {
    pub first: u16,
    pub second: Option<u16>,
}

/// Resolve Stereo Link flags into groups, scanning top to bottom.
///
/// `link[ch]` links channel `ch` with the channel below it. A channel that is
/// already the lower half of a link cannot itself link further; its own flag is
/// ignored (and kept, so it comes back if the link above is removed).
pub fn link_groups(channels: u16, link: &[bool]) -> Vec<LinkGroup> {
    let mut out = Vec::new();
    let mut ch = 0u16;
    while ch < channels {
        if ch + 1 < channels && link.get(ch as usize).copied().unwrap_or(false) {
            out.push(LinkGroup {
                first: ch,
                second: Some(ch + 1),
            });
            ch += 2;
        } else {
            out.push(LinkGroup {
                first: ch,
                second: None,
            });
            ch += 1;
        }
    }
    out
}

/// Whether the Stereo Link option is shown on `ch`:
/// * there is a channel below it,
/// * `ch` is not already the lower half of a link from the channel above, and
/// * the channel below is not itself stereo linked to the next one.
pub fn can_link(channels: u16, link: &[bool], ch: u16) -> bool {
    let groups = link_groups(channels, link);
    let starts_group = groups.iter().any(|g| g.first == ch);
    let next_is_linked = groups
        .iter()
        .any(|g| g.first == ch + 1 && g.second.is_some());
    ch + 1 < channels && starts_group && !next_is_linked
}

/// Linked channels are selected together, like stereo-linked mixer channels.
/// After links change, a pair whose two channels disagree becomes selected
/// (linking signals that the pair is wanted as stereo).
pub fn sync_linked_selection(channels: u16, link: &[bool], selected: &mut [bool]) {
    for g in link_groups(channels, link) {
        if let Some(b) = g.second {
            let (a, b) = (g.first as usize, b as usize);
            if selected[a] != selected[b] {
                selected[a] = true;
                selected[b] = true;
            }
        }
    }
}

/// Output channels for one group given the selection: a stereo pair when both
/// linked channels are selected, otherwise mono files for what is selected.
pub fn group_outputs(g: LinkGroup, selected: &[bool]) -> Vec<Vec<u16>> {
    let sel = |c: u16| selected.get(c as usize).copied().unwrap_or(false);
    match g.second {
        Some(b) if sel(g.first) && sel(b) => vec![vec![g.first, b]],
        Some(b) => [g.first, b]
            .into_iter()
            .filter(|&c| sel(c))
            .map(|c| vec![c])
            .collect(),
        None if sel(g.first) => vec![vec![g.first]],
        None => vec![],
    }
}

/// Build the list of output files from the selection and Stereo Link flags.
pub fn plan_outputs(
    base: &Path,
    channels: u16,
    selected: &[bool],
    link: &[bool],
) -> Vec<OutputSpec> {
    link_groups(channels, link)
        .into_iter()
        .flat_map(|g| group_outputs(g, selected))
        .map(|g| OutputSpec {
            path: output_path(base, &g),
            channels: g,
        })
        .collect()
}

fn partial_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Split `input` into the given outputs. Sample data is copied bit-for-bit.
///
/// `progress` receives the number of frames processed so far. Setting `cancel`
/// aborts the split; partially written files are removed and existing files
/// with the same names are left untouched.
pub fn split(
    input: &Path,
    outputs: &[OutputSpec],
    progress: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    let info = read_info(input)?;
    if outputs.is_empty() {
        bail!("No channels selected.");
    }
    for o in outputs {
        if o.channels.is_empty() || o.channels.iter().any(|&c| c >= info.format.channels) {
            bail!("Invalid channel selection for {}.", o.path.display());
        }
    }

    if let Ok(canon_in) = input.canonicalize() {
        if outputs
            .iter()
            .any(|o| o.path.canonicalize().ok().as_ref() == Some(&canon_in))
        {
            bail!("An output file would overwrite the input file.");
        }
    }

    let parts: Vec<PathBuf> = outputs.iter().map(|o| partial_path(&o.path)).collect();
    let result = split_inner(input, &info, outputs, &parts, progress, cancel);

    match result {
        Ok(()) => {
            for (part, o) in parts.iter().zip(outputs) {
                // `rename` replaces an existing file on all platforms we target.
                fs::rename(part, &o.path)
                    .with_context(|| format!("Could not write {}", o.path.display()))?;
            }
            Ok(())
        }
        Err(e) => {
            for part in &parts {
                let _ = fs::remove_file(part);
            }
            Err(e)
        }
    }
}

#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cancelled.")
    }
}
impl std::error::Error for Cancelled {}

fn split_inner(
    input: &Path,
    info: &WavInfo,
    outputs: &[OutputSpec],
    parts: &[PathBuf],
    progress: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    let bps = info.format.bytes_per_sample();
    let block = info.format.block_align();

    struct Out {
        writer: WavWriter,
        offsets: Vec<usize>,
        buf: Vec<u8>,
    }

    let mut outs = Vec::with_capacity(outputs.len());
    for (o, part) in outputs.iter().zip(parts) {
        let fmt = WavFormat {
            channels: o.channels.len() as u16,
            ..info.format.clone()
        };
        outs.push(Out {
            writer: WavWriter::create(part, fmt)?,
            offsets: o.channels.iter().map(|&c| c as usize * bps).collect(),
            buf: Vec::new(),
        });
    }

    let mut f = File::open(input)?;
    f.seek(SeekFrom::Start(info.data_offset))?;

    let frames_per_chunk = ((8 << 20) / block).max(1);
    let mut inbuf = vec![0u8; frames_per_chunk * block];
    let total = info.frames();
    let mut done = 0u64;

    while done < total {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let n = ((total - done) as usize).min(frames_per_chunk);
        let chunk = &mut inbuf[..n * block];
        f.read_exact(chunk)
            .context("Unexpected end of file while reading audio.")?;

        for out in &mut outs {
            out.buf.clear();
            out.buf.reserve(n * out.offsets.len() * bps);
            for frame in chunk.chunks_exact(block) {
                for &off in &out.offsets {
                    out.buf.extend_from_slice(&frame[off..off + bps]);
                }
            }
            out.writer.write(&out.buf)?;
        }
        done += n as u64;
        progress.store(done, Ordering::Relaxed);
    }

    for out in outs {
        out.writer.finalize()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::SampleKind;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn labels_and_paths() {
        assert_eq!(channel_label(&[0]), "CH 1");
        assert_eq!(channel_label(&[2, 3]), "CH 3-4");
        assert_eq!(default_base(&p("/rec/Show 1.wav")), p("/rec/Show 1"));
        assert_eq!(base_from_user_path(&p("/x/Mix.WAV")), p("/x/Mix"));
        assert_eq!(base_from_user_path(&p("/x/Mix")), p("/x/Mix"));
        assert_eq!(
            output_path(&p("/rec/Show"), &[0, 1]),
            p("/rec/Show CH 1-2.wav")
        );
    }

    #[test]
    fn planning() {
        let base = p("/r/S");
        let names = |v: Vec<OutputSpec>| {
            v.into_iter()
                .map(|o| channel_label(&o.channels))
                .collect::<Vec<_>>()
        };
        // Defaults: 1-2, 3-4 linked, 5 alone.
        assert_eq!(default_links(5), [true, false, true, false, false]);
        assert_eq!(
            names(plan_outputs(&base, 5, &[true; 5], &default_links(5))),
            ["CH 1-2", "CH 3-4", "CH 5"]
        );
        // Unlink 3-4.
        assert_eq!(
            names(plan_outputs(
                &base,
                5,
                &[true; 5],
                &[true, false, false, false, false]
            )),
            ["CH 1-2", "CH 3", "CH 4", "CH 5"]
        );
        // Only one channel of a linked pair selected -> mono.
        assert_eq!(
            names(plan_outputs(
                &base,
                4,
                &[false, true, true, false],
                &default_links(4)
            )),
            ["CH 2", "CH 3"]
        );
        assert!(plan_outputs(&base, 4, &[false; 4], &default_links(4)).is_empty());
    }

    #[test]
    fn shifting_links() {
        let base = p("/r/S");
        let names = |v: Vec<OutputSpec>| {
            v.into_iter()
                .map(|o| channel_label(&o.channels))
                .collect::<Vec<_>>()
        };
        // 3 channels: 1-2 linked; link option only on CH 1.
        let mut link = default_links(3);
        assert!(can_link(3, &link, 0) && !can_link(3, &link, 1) && !can_link(3, &link, 2));
        // Unlink CH 1 -> option appears on CH 2; link 2-3.
        link[0] = false;
        assert!(can_link(3, &link, 1));
        link[1] = true;
        assert_eq!(
            names(plan_outputs(&base, 3, &[true; 3], &link)),
            ["CH 1", "CH 2-3"]
        );
        // CH 2 is linked on, so CH 1 cannot link.
        assert!(!can_link(3, &link, 0));
        // Unlink CH 2 -> CH 1 can link again.
        link[1] = false;
        assert!(can_link(3, &link, 0) && can_link(3, &link, 1));
        link[0] = true;
        assert_eq!(
            names(plan_outputs(&base, 3, &[true; 3], &link)),
            ["CH 1-2", "CH 3"]
        );
        assert!(!can_link(3, &link, 1));
        // Linking a half-selected pair selects both channels.
        let mut sel = [true, true, false];
        sync_linked_selection(3, &[false, true, false], &mut sel);
        assert_eq!(sel, [true, true, true]);
        // Last channel can never link.
        assert!(!can_link(3, &[false, false, true], 2));
    }

    #[test]
    fn split_is_bit_exact() {
        let dir = std::env::temp_dir().join(format!("mws-split-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("multi.wav");
        let fmt = WavFormat {
            channels: 5,
            sample_rate: 44100,
            container_bits: 24,
            valid_bits: 24,
            kind: SampleKind::Int,
        };
        let frames = 12345usize;
        // Sample value encodes (channel, frame) so misplacement is detectable.
        let mut data = Vec::new();
        for fr in 0..frames {
            for ch in 0..5u32 {
                let v = (ch << 20) | (fr as u32 & 0xFFFFF);
                data.extend_from_slice(&v.to_le_bytes()[..3]);
            }
        }
        let mut w = WavWriter::create(&input, fmt).unwrap();
        w.write(&data).unwrap();
        w.finalize().unwrap();

        let base = default_base(&input);
        let outs = plan_outputs(
            &base,
            5,
            &[true, true, true, true, true],
            &[true, false, false, false, false],
        );
        let progress = AtomicU64::new(0);
        split(&input, &outs, &progress, &AtomicBool::new(false)).unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), frames as u64);

        for o in &outs {
            let info = read_info(&o.path).unwrap();
            assert_eq!(info.format.channels as usize, o.channels.len());
            assert_eq!(info.frames(), frames as u64);
            let bytes = std::fs::read(&o.path).unwrap();
            let d = &bytes[info.data_offset as usize..];
            let mut i = 0;
            for fr in 0..frames {
                for &ch in &o.channels {
                    let v = u32::from_le_bytes([d[i], d[i + 1], d[i + 2], 0]);
                    assert_eq!(v, ((ch as u32) << 20) | (fr as u32 & 0xFFFFF));
                    i += 3;
                }
            }
            assert!(!partial_path(&o.path).exists());
        }
        let names: Vec<String> = outs
            .iter()
            .map(|o| o.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "multi CH 1-2.wav",
                "multi CH 3.wav",
                "multi CH 4.wav",
                "multi CH 5.wav"
            ]
        );

        // Cancelling leaves no files behind and does not touch existing ones.
        std::fs::write(&outs[0].path, b"keep").unwrap();
        let err = split(&input, &outs, &AtomicU64::new(0), &AtomicBool::new(true)).unwrap_err();
        assert!(err.downcast_ref::<Cancelled>().is_some());
        assert_eq!(std::fs::read(&outs[0].path).unwrap(), b"keep");
        assert!(outs.iter().all(|o| !partial_path(&o.path).exists()));

        // Refuses to overwrite the input.
        let bad = vec![OutputSpec {
            channels: vec![0],
            path: input.clone(),
        }];
        assert!(split(&input, &bad, &AtomicU64::new(0), &AtomicBool::new(false)).is_err());
    }
}
