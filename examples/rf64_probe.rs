//! Writes a small forced-RF64 file for interoperability checks.
use multiwave_splitter::wav::{SampleKind, WavFormat, WavWriter};
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("path");
    let f = WavFormat {
        channels: 2,
        sample_rate: 48000,
        container_bits: 24,
        valid_bits: 24,
        kind: SampleKind::Int,
    };
    let mut w = WavWriter::create(path.as_ref(), f)?;
    w.force_rf64();
    let data: Vec<u8> = (0..48000 * 6).map(|i| (i % 200) as u8).collect();
    w.write(&data)?;
    w.finalize()
}
