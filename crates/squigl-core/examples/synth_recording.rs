//! Writes a recording of the test pattern (moving colour bars), encoded with
//! openh264, for anything that wants a `.sqrec` with no phone: the desktop app's
//! end-to-end tests, and frame-rate checks at a size the phone may not offer.
//!
//! ```text
//! cargo run --release -p squigl-core --example synth_recording -- OUT [WxH] [SECONDS]
//! ```
//!
//! The default is 1280x720 for 4 s, at the pattern's 30 frames per second, with a
//! key frame every second so a looping replay can start anywhere.

use openh264::encoder::Encoder;
use openh264::formats::YUVBuffer;
use squigl_core::protocol::{CodecMeta, FramePacket};
use squigl_core::replay::Recorder;
use squigl_core::test_pattern::TestPattern;
use std::fs::File;
use std::io::BufWriter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: synth_recording OUT [WxH] [SECONDS]";
    let out = args.next().ok_or(usage)?;
    let (w, h) = match args.next() {
        Some(size) => {
            let (w, h) = size.split_once('x').ok_or(usage)?;
            (w.parse::<usize>()?, h.parse::<usize>()?)
        }
        None => (1280, 720),
    };
    let seconds: u32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(4);

    let pattern = TestPattern::new(w, h);
    let fps = TestPattern::FPS;
    let mut encoder = Encoder::new()?;
    let mut recorder = Recorder::new(
        BufWriter::new(File::create(&out)?),
        CodecMeta {
            width: w as u32,
            height: h as u32,
        },
    )?;
    for i in 0..(seconds * fps) as usize {
        let key = i % fps as usize == 0;
        if key {
            encoder.force_intra_frame();
        }
        let frame = pattern.frame(i);
        let yuv = [frame.y, frame.u, frame.v].concat();
        let data = encoder.encode(&YUVBuffer::from_vec(yuv, w, h))?.to_vec();
        recorder.packet(&FramePacket {
            is_config: false,
            is_key_frame: key,
            pts_us: i as u64 * 1_000_000 / u64::from(fps),
            data,
        })?;
    }
    recorder.finish()?;
    println!("wrote {out}: {w}x{h}, {seconds} s");
    Ok(())
}
