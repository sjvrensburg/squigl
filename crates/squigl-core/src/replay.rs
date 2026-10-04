//! A camera stream recorded to a file ([`Recorder`]) and played back as a source
//! ([`Replay`]), so the pipeline and the front ends run with no phone: in CI, in
//! demos, and in tests that need real frames.
//!
//! The format is squigl's own rather than scrcpy's wire format, which is undocumented
//! and changes between versions. It is:
//!
//! - [`MAGIC`] (8 bytes);
//! - the frame width and height (`u32` LE each);
//! - one record per H.264 packet: pts in microseconds (`u64` LE), flags (`u8`: bit 0
//!   config, bit 1 key frame), payload length (`u32` LE), then the Annex-B payload.
//!
//! A recording starts at the stream's first packet (SPS/PPS, then a key frame), so
//! it decodes from its start, and a loop back to the start decodes cleanly too.

use crate::decode::{Backend, Decoder};
use crate::error::{Error, Result};
use crate::protocol::{CodecMeta, FramePacket};
use crate::sink::FrameSink;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The first bytes of a recording; the trailing digit is the format version.
pub const MAGIC: &[u8; 8] = b"SQGLREC1";

const HEADER_LEN: u64 = 16;
const FLAG_CONFIG: u8 = 1;
const FLAG_KEY_FRAME: u8 = 2;
/// Larger than any real packet (a 4000x3000 key frame at 30 Mbit/s is ~1 MB); a
/// length beyond it means the file is not what its header says.
const MAX_PACKET: usize = 64 << 20;

/// Writes a recording: the header at creation, then [`packet`](Self::packet) per
/// packet in stream order.
pub struct Recorder<W: Write> {
    out: W,
}

impl<W: Write> Recorder<W> {
    /// Starts a recording of a stream of `meta`'s size.
    pub fn new(mut out: W, meta: CodecMeta) -> Result<Self> {
        out.write_all(MAGIC)?;
        out.write_all(&meta.width.to_le_bytes())?;
        out.write_all(&meta.height.to_le_bytes())?;
        Ok(Self { out })
    }

    pub fn packet(&mut self, packet: &FramePacket) -> Result<()> {
        let flags = if packet.is_config { FLAG_CONFIG } else { 0 }
            | if packet.is_key_frame {
                FLAG_KEY_FRAME
            } else {
                0
            };
        let len = u32::try_from(packet.data.len())
            .map_err(|_| Error::Recording("packet too large to record".to_string()))?;
        self.out.write_all(&packet.pts_us.to_le_bytes())?;
        self.out.write_all(&[flags])?;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(&packet.data)?;
        Ok(())
    }

    /// Flushes and returns the writer.
    pub fn finish(mut self) -> Result<W> {
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Plays a recording back: [`run`](Self::run) decodes it into a [`FrameSink`] at the
/// pace it was recorded, like [`crate::CameraSession::run`] does a live stream.
pub struct Replay<R> {
    input: R,
    decoder_backend: Backend,
    looping: bool,
    frames_decoded: u64,
    pub meta: CodecMeta,
}

impl Replay<BufReader<File>> {
    pub fn open(path: &Path, decoder: Backend) -> Result<Self> {
        let file = File::open(path)
            .map_err(|e| Error::Recording(format!("opening recording {}: {e}", path.display())))?;
        Self::new(BufReader::new(file), decoder)
    }
}

impl<R: Read + Seek> Replay<R> {
    /// Reads the header; fails if `input` is not a recording.
    pub fn new(mut input: R, decoder: Backend) -> Result<Self> {
        let mut header = [0u8; HEADER_LEN as usize];
        input
            .read_exact(&mut header)
            .map_err(|_| Error::Recording("too short to be a squigl recording".to_string()))?;
        if &header[..8] != MAGIC {
            return Err(Error::Recording(
                "not a squigl recording (made with `squigl-cli --record`)".to_string(),
            ));
        }
        let meta = CodecMeta {
            width: u32::from_le_bytes(header[8..12].try_into().unwrap()),
            height: u32::from_le_bytes(header[12..16].try_into().unwrap()),
        };
        Ok(Self {
            input,
            decoder_backend: decoder,
            looping: false,
            frames_decoded: 0,
            meta,
        })
    }

    /// Starts over from the beginning at the end, until stopped.
    pub fn looping(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }

    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded
    }

    /// Blocks, decoding the recording and handing each frame to `sink` at the pace
    /// it was recorded, until:
    ///
    /// - `stop` becomes true (checked at least every 100 ms) -> `Ok(())`;
    /// - the recording ends and is not [looping](Self::looping) -> `Ok(())`;
    /// - any error, including a pass through the recording that decodes nothing.
    ///
    /// A recording cut off mid-packet (the recorder was killed) ends at the last
    /// whole packet.
    pub fn run<S: FrameSink + ?Sized>(&mut self, sink: &mut S, stop: &AtomicBool) -> Result<()> {
        let mut decoder = Decoder::with_backend(self.decoder_backend)?;
        loop {
            let decoded = self.pass(&mut decoder, sink, stop)?;
            if stop.load(Ordering::Relaxed) || !self.looping {
                return Ok(());
            }
            if decoded == 0 {
                return Err(Error::Recording(format!(
                    "no frame decoded from the recording with the {} decoder",
                    decoder.backend().name()
                )));
            }
            self.input.seek(SeekFrom::Start(HEADER_LEN))?;
        }
    }

    /// Plays the recording once from the current position; returns the frames
    /// decoded.
    fn pass<S: FrameSink + ?Sized>(
        &mut self,
        decoder: &mut Decoder,
        sink: &mut S,
        stop: &AtomicBool,
    ) -> Result<u64> {
        // Undecodable packets are skipped, as a live session does; only a run of them
        // with nothing in between ends playback.
        const MAX_CONSECUTIVE_DECODE_ERRORS: u32 = 300;
        let mut consecutive_errors = 0;
        let mut decoded = 0;
        // When the first timed packet was due, and its pts.
        let mut clock: Option<(Instant, u64)> = None;
        while !stop.load(Ordering::Relaxed) {
            let Some(packet) = self.read_packet()? else {
                break;
            };
            if !packet.is_config {
                match clock {
                    None => clock = Some((Instant::now(), packet.pts_us)),
                    Some((start, first)) => {
                        let due =
                            start + Duration::from_micros(packet.pts_us.saturating_sub(first));
                        if !sleep_until(due, stop) {
                            break;
                        }
                    }
                }
            }
            match decoder.decode(&packet.data) {
                Ok(Some(frame)) => {
                    consecutive_errors = 0;
                    decoded += 1;
                    self.frames_decoded += 1;
                    sink.frame(&frame)?;
                }
                Ok(None) => {}
                Err(e) => {
                    consecutive_errors += 1;
                    if consecutive_errors >= MAX_CONSECUTIVE_DECODE_ERRORS {
                        return Err(e);
                    }
                    log::debug!("skipping undecodable packet ({e})");
                }
            }
        }
        Ok(decoded)
    }

    /// The next packet, or `None` at the end (including a packet cut off midway).
    fn read_packet(&mut self) -> Result<Option<FramePacket>> {
        let mut head = [0u8; 13];
        match read_full(&mut self.input, &mut head)? {
            0 => return Ok(None),
            13 => {}
            _ => {
                log::warn!("recording ends mid-packet; playing what came before");
                return Ok(None);
            }
        }
        let pts_us = u64::from_le_bytes(head[..8].try_into().unwrap());
        let flags = head[8];
        let len = u32::from_le_bytes(head[9..13].try_into().unwrap()) as usize;
        if len > MAX_PACKET {
            return Err(Error::Recording(format!(
                "recording is damaged (a {len}-byte packet)"
            )));
        }
        let mut data = vec![0u8; len];
        if read_full(&mut self.input, &mut data)? < len {
            log::warn!("recording ends mid-packet; playing what came before");
            return Ok(None);
        }
        Ok(Some(FramePacket {
            is_config: flags & FLAG_CONFIG != 0,
            is_key_frame: flags & FLAG_KEY_FRAME != 0,
            pts_us,
            data,
        }))
    }
}

/// Reads until `buf` is full or the input ends; returns the bytes read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Sleeps until `due`, waking every 100 ms to check `stop`; false if stopped.
fn sleep_until(due: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= due {
            return true;
        }
        std::thread::sleep((due - now).min(Duration::from_millis(100)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::YuvFrame;
    use openh264::encoder::Encoder;
    use openh264::formats::YUVBuffer;
    use std::io::Cursor;

    /// `n` frames of a `w`x`h` stream, `step_us` apart, encoded with openh264 and
    /// recorded; frame `i` is a flat grey of luma `16 + 20 i`.
    fn recording(w: u32, h: u32, n: u8, step_us: u64) -> Vec<u8> {
        let mut encoder = Encoder::new().unwrap();
        let mut rec = Recorder::new(
            Vec::new(),
            CodecMeta {
                width: w,
                height: h,
            },
        )
        .unwrap();
        let (w, h) = (w as usize, h as usize);
        for i in 0..n {
            let mut yuv = vec![128u8; w * h * 3 / 2];
            yuv[..w * h].fill(16 + 20 * i);
            let data = encoder
                .encode(&YUVBuffer::from_vec(yuv, w, h))
                .unwrap()
                .to_vec();
            rec.packet(&FramePacket {
                is_config: false,
                is_key_frame: i == 0,
                pts_us: u64::from(i) * step_us,
                data,
            })
            .unwrap();
        }
        rec.finish().unwrap()
    }

    /// Plays `bytes` into a vector of (size, mean luma) until it ends or `limit`
    /// frames have arrived.
    fn play(bytes: Vec<u8>, looping: bool, limit: usize) -> Vec<((usize, usize), u8)> {
        let mut replay = Replay::new(Cursor::new(bytes), Backend::default())
            .unwrap()
            .looping(looping);
        let stop = AtomicBool::new(false);
        let mut seen = Vec::new();
        let mut sink = |f: &YuvFrame| {
            let mean = f.y.iter().map(|&p| u64::from(p)).sum::<u64>() / f.y.len() as u64;
            seen.push(((f.width, f.height), mean as u8));
            if seen.len() >= limit {
                stop.store(true, Ordering::Relaxed);
            }
            Ok(())
        };
        replay.run(&mut sink, &stop).unwrap();
        seen
    }

    #[test]
    fn a_recording_plays_back_every_frame_in_order() {
        let seen = play(recording(64, 48, 5, 1_000), false, usize::MAX);
        assert_eq!(seen.len(), 5);
        for (i, &(size, luma)) in seen.iter().enumerate() {
            assert_eq!(size, (64, 48));
            let want = 16 + 20 * i as i32;
            assert!(
                (i32::from(luma) - want).abs() <= 2,
                "frame {i}: {luma} vs {want}"
            );
        }
    }

    #[test]
    fn a_looping_replay_starts_over_until_stopped() {
        let seen = play(recording(64, 48, 3, 1_000), true, 7);
        assert_eq!(seen.len(), 7);
        // Frame 3 is the first frame again.
        assert!(seen[3].1.abs_diff(seen[0].1) <= 2, "{seen:?}");
    }

    #[test]
    fn playback_keeps_the_recorded_pace() {
        let t = Instant::now();
        play(recording(64, 48, 4, 30_000), false, usize::MAX);
        // Three 30 ms gaps between four frames.
        assert!(
            t.elapsed() >= Duration::from_millis(90),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn a_recording_cut_off_mid_packet_plays_what_came_before() {
        let mut bytes = recording(64, 48, 3, 1_000);
        bytes.truncate(bytes.len() - 5);
        assert_eq!(play(bytes, false, usize::MAX).len(), 2);
    }

    #[test]
    fn the_header_is_checked() {
        let bytes = recording(64, 48, 1, 0);
        let replay = Replay::new(Cursor::new(bytes.clone()), Backend::default()).unwrap();
        assert_eq!(
            replay.meta,
            CodecMeta {
                width: 64,
                height: 48
            }
        );
        let mut other = bytes;
        other[0] = b'X';
        assert!(matches!(
            Replay::new(Cursor::new(other), Backend::default()),
            Err(Error::Recording(_))
        ));
        assert!(matches!(
            Replay::new(Cursor::new(b"SQGL".to_vec()), Backend::default()),
            Err(Error::Recording(_))
        ));
    }

    #[test]
    fn a_recording_that_decodes_nothing_does_not_loop_forever() {
        let mut rec = Recorder::new(
            Vec::new(),
            CodecMeta {
                width: 64,
                height: 48,
            },
        )
        .unwrap();
        rec.packet(&FramePacket {
            is_config: false,
            is_key_frame: false,
            pts_us: 0,
            data: vec![0, 0, 0, 1, 0x65, 0xff],
        })
        .unwrap();
        let bytes = rec.finish().unwrap();
        let mut replay = Replay::new(Cursor::new(bytes), Backend::default())
            .unwrap()
            .looping(true);
        let mut sink = |_: &YuvFrame| Ok(());
        assert!(replay.run(&mut sink, &AtomicBool::new(false)).is_err());
    }
}
