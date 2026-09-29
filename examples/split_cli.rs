//! Headless split, mainly for testing:
//!   cargo run --release --example split_cli -- input.wav 1-2 3 4 [--out /dir/base]
use multiwave_splitter::split::{default_base, output_path, split, OutputSpec};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(
        args.next()
            .expect("usage: split_cli input.wav 1-2 3 ... [--out base]"),
    );
    let mut base = default_base(&input);
    let mut outputs = Vec::new();
    let mut groups = Vec::new();
    while let Some(a) = args.next() {
        if a == "--out" {
            base = PathBuf::from(args.next().expect("--out needs a value"));
        } else {
            let chans: Vec<u16> = a
                .split('-')
                .map(|c| c.parse::<u16>().unwrap() - 1)
                .collect();
            groups.push(chans);
        }
    }
    for g in groups {
        outputs.push(OutputSpec {
            path: output_path(&base, &g),
            channels: g,
        });
    }
    let info = multiwave_splitter::wav::read_info(&input)?;
    println!("{:?}, {} frames", info.format, info.frames());
    split(
        &input,
        &outputs,
        &AtomicU64::new(0),
        &AtomicBool::new(false),
    )?;
    for o in outputs {
        println!("wrote {}", o.path.display());
    }
    Ok(())
}
