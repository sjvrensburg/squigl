//! A phone's browser for tests (feature `testing`): str0m offers H.264, send-only,
//! from 127.0.0.1 and, once connected, sends openh264-encoded colour bars about 30
//! times a second until dropped.

use squigl_core::test_pattern::TestPattern;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};

pub struct FakePhone {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakePhone {
    /// Sends the offer to `answer` -- a WHIP POST, or
    /// `WebrtcSource::accept_offer` itself -- and streams `w`x`h` frames once the
    /// answer connects.
    pub fn connect(
        (w, h): (usize, usize),
        answer: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        let local = socket.local_addr().map_err(|e| e.to_string())?;
        let mut rtc = Rtc::new(Instant::now());
        rtc.add_local_candidate(Candidate::host(local, "udp").map_err(|e| e.to_string())?);
        let mut change = rtc.sdp_api();
        let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().ok_or("no offer")?;
        let reply = answer(&offer.to_sdp_string())?;
        let reply = str0m::change::SdpAnswer::from_sdp_string(&reply).map_err(|e| e.to_string())?;
        rtc.sdp_api()
            .accept_answer(pending, reply)
            .map_err(|e| e.to_string())?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                let pattern = TestPattern::new(w, h);
                let mut encoder = openh264::encoder::Encoder::new().expect("an encoder");
                let mut connected = false;
                let mut sent = 0usize;
                let mut next_frame = Instant::now();
                let mut buf = [0u8; 2000];
                while !stop.load(Ordering::Relaxed) {
                    if connected && Instant::now() >= next_frame {
                        let frame = pattern.frame(sent);
                        let yuv = [frame.y, frame.u, frame.v].concat();
                        let data = encoder
                            .encode(&openh264::formats::YUVBuffer::from_vec(yuv, w, h))
                            .expect("encoding")
                            .to_vec();
                        let writer = rtc.writer(mid).expect("the video");
                        let pt = writer
                            .payload_params()
                            .find(|p| p.spec().codec == Codec::H264)
                            .expect("H.264 negotiated")
                            .pt();
                        let time = MediaTime::new(sent as u64 * 3000, Frequency::NINETY_KHZ);
                        writer
                            .write(pt, Instant::now(), time, data)
                            .expect("writing");
                        sent += 1;
                        next_frame += Duration::from_millis(33);
                    }
                    let timeout = match rtc.poll_output().expect("str0m") {
                        Output::Transmit(t) => {
                            let _ = socket.send_to(&t.contents, t.destination);
                            continue;
                        }
                        Output::Event(Event::Connected) => {
                            connected = true;
                            next_frame = Instant::now();
                            continue;
                        }
                        Output::Event(_) => continue,
                        Output::Timeout(t) => t,
                    };
                    let wait = timeout
                        .min(next_frame)
                        .saturating_duration_since(Instant::now())
                        .clamp(Duration::from_millis(1), Duration::from_millis(50));
                    socket.set_read_timeout(Some(wait)).expect("a timeout");
                    let input = match socket.recv_from(&mut buf) {
                        Ok((n, source)) => {
                            match Receive::new(Protocol::Udp, source, local, &buf[..n]) {
                                Ok(receive) => Input::Receive(Instant::now(), receive),
                                Err(_) => continue,
                            }
                        }
                        Err(_) => Input::Timeout(Instant::now()),
                    };
                    rtc.handle_input(input).expect("str0m");
                }
            }
        });
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for FakePhone {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
