//! WebRTC camera source: an alternative to [`crate::CameraSession`] for phones that
//! stream over the network instead of ADB. There is no squigl-side app to push here
//! (unlike scrcpy-server) -- the phone side is a browser page doing `getUserMedia` and
//! pushing the offer over HTTP (WHIP-style: one POST with an SDP offer, one SDP answer
//! back). This module only speaks the WebRTC/ICE/DTLS/SRTP half of that exchange, via
//! `str0m` (a sans-I/O implementation, matching this crate's synchronous style); the
//! HTTP server and capture page are the caller's job (`squigl-cli`/`squigl`'s "policy"
//! layer), same division as [`crate::adb`] vs. [`crate::session`].
//!
//! LAN-only by design: no STUN/TURN, just a host ICE candidate on the interface the
//! caller binds to. A phone on a different network (cellular, guest Wi-Fi) won't reach
//! this without a relay, which is out of scope -- see the crate README.
//!
//! H.264 only ([`accept_offer`] restricts the SDP negotiation to it), so the browser
//! page must prefer/force H.264 in its `getUserMedia`/transceiver setup: str0m's H.264
//! depacketizer hands back complete Annex-B access units (start-code delimited, exactly
//! what [`crate::decode::Decoder::decode`] already expects from the scrcpy protocol
//! path), so no format conversion is needed between the two sources.

use crate::decode::{self, Decoder, YuvFrame};
use crate::error::{Error, Result};
use crate::session::STALL_TIMEOUT;
use crate::sink::{FrameSink, V4l2Sink};
use std::net::{IpAddr, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

/// Max UDP datagram str0m expects to handle (matches its own internal ceiling).
const RECV_BUF: usize = 2000;

pub struct WebrtcSource {
    rtc: Rtc,
    socket: UdpSocket,
    decoder: Decoder,
    connected: bool,
    last_activity: Instant,
    /// When the offer was accepted, used to bound how long we'll wait for ICE/DTLS to
    /// reach [`Event::Connected`] before giving up -- unlike the ADB path, nothing else
    /// here times out a peer that never finishes connecting (e.g. blocked by a firewall,
    /// or the wrong network), which would otherwise hang the session thread forever and
    /// leave the server's one-session-at-a-time slot stuck.
    accept_time: Instant,
}

impl WebrtcSource {
    /// Accepts a browser's SDP offer (the WHIP POST body), binds a UDP socket on
    /// `bind_addr` for the media itself, and returns the session plus the SDP answer
    /// to send back (the WHIP response body). Media only starts flowing once the
    /// caller drives [`Self::run`]/[`Self::run_to_v4l2`] -- ICE and DTLS complete
    /// there, not here.
    pub fn accept_offer(
        offer_sdp: &str,
        bind_addr: IpAddr,
        decoder_backend: decode::Backend,
    ) -> Result<(Self, String)> {
        let offer = SdpOffer::from_sdp_string(offer_sdp)
            .map_err(|e| Error::Protocol(format!("invalid SDP offer: {e}")))?;

        let mut rtc = RtcConfig::new()
            .clear_codecs()
            .enable_h264(true)
            .build(Instant::now());

        let socket = UdpSocket::bind((bind_addr, 0))?;
        let local_addr = socket.local_addr()?;
        let candidate = Candidate::host(local_addr, "udp")
            .map_err(|e| Error::Protocol(format!("building ICE candidate: {e}")))?;
        rtc.add_local_candidate(candidate);

        let answer: SdpAnswer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| Error::Protocol(format!("accepting SDP offer: {e}")))?;

        let decoder = Decoder::with_backend(decoder_backend)?;

        Ok((
            Self {
                rtc,
                socket,
                decoder,
                connected: false,
                last_activity: Instant::now(),
                accept_time: Instant::now(),
            },
            answer.to_sdp_string(),
        ))
    }

    /// Blocks, decoding the incoming stream and handing each frame to `sink`, until:
    ///
    /// - `stop` becomes true -> `Ok(())`;
    /// - the peer disconnects -> `Ok(())`;
    /// - ICE/DTLS never reaches [`Event::Connected`] within [`STALL_TIMEOUT`] of the
    ///   offer being accepted -> [`Error::StreamStalled`];
    /// - nothing decodes for [`STALL_TIMEOUT`] after the connection was ever
    ///   established -> [`Error::StreamStalled`];
    /// - any other error.
    pub fn run<S: FrameSink + ?Sized>(&mut self, sink: &mut S, stop: &AtomicBool) -> Result<()> {
        while let Some(frame) = self.next_frame(stop)? {
            sink.frame(&frame)?;
        }
        Ok(())
    }

    /// Like [`Self::run`], but opens the V4L2 sink itself once the first frame's size
    /// is known -- unlike [`crate::CameraSession`], there's no upfront "session meta"
    /// telling us the size before the stream starts (the browser's `getUserMedia`
    /// resolution isn't announced any other way).
    pub fn run_to_v4l2(&mut self, device_path: &Path, stop: &AtomicBool) -> Result<()> {
        let Some(first) = self.next_frame(stop)? else {
            return Ok(());
        };
        let mut sink = V4l2Sink::open(device_path, first.width as u32, first.height as u32)?;
        sink.frame(&first)?;
        while let Some(frame) = self.next_frame(stop)? {
            sink.frame(&frame)?;
        }
        Ok(())
    }

    /// Drives str0m (UDP I/O, ICE, DTLS, RTP reassembly) until one frame decodes,
    /// `stop` is raised, or the peer disconnects.
    fn next_frame(&mut self, stop: &AtomicBool) -> Result<Option<YuvFrame>> {
        let mut buf = [0u8; RECV_BUF];
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }

            let timeout = match self.rtc.poll_output().map_err(str0m_err)? {
                Output::Timeout(t) => t,
                Output::Transmit(t) => {
                    // Best-effort: a dropped reply (e.g. a stale candidate pair) isn't
                    // fatal, str0m will retry or move on via ICE.
                    let _ = self.socket.send_to(&t.contents, t.destination);
                    continue;
                }
                Output::Event(Event::Connected) => {
                    self.connected = true;
                    self.last_activity = Instant::now();
                    continue;
                }
                Output::Event(Event::IceConnectionStateChange(
                    IceConnectionState::Disconnected,
                )) if self.connected => {
                    return Ok(None);
                }
                Output::Event(Event::MediaData(data)) => {
                    self.last_activity = Instant::now();
                    match self.decoder.decode(&data.data) {
                        Ok(Some(frame)) => return Ok(Some(frame)),
                        Ok(None) => continue,
                        Err(e) => {
                            log::debug!("skipping undecodable WebRTC frame ({e})");
                            continue;
                        }
                    }
                }
                Output::Event(_) => continue,
            };

            let wait = timeout
                .saturating_duration_since(Instant::now())
                .clamp(Duration::from_millis(1), Duration::from_millis(500));
            self.socket.set_read_timeout(Some(wait))?;

            match self.socket.recv_from(&mut buf) {
                Ok((n, source)) => {
                    let destination = self.socket.local_addr()?;
                    let recv = Receive::new(Protocol::Udp, source, destination, &buf[..n])
                        .map_err(|e| Error::Protocol(format!("bad datagram: {e}")))?;
                    self.rtc
                        .handle_input(Input::Receive(Instant::now(), recv))
                        .map_err(str0m_err)?;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    self.rtc
                        .handle_input(Input::Timeout(Instant::now()))
                        .map_err(str0m_err)?;
                    if self.connected && self.last_activity.elapsed() > STALL_TIMEOUT {
                        return Err(Error::StreamStalled(STALL_TIMEOUT));
                    }
                    if !self.connected && self.accept_time.elapsed() > STALL_TIMEOUT {
                        return Err(Error::StreamStalled(STALL_TIMEOUT));
                    }
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }
}

fn str0m_err(e: str0m::RtcError) -> Error {
    Error::Protocol(format!("WebRTC error: {e}"))
}
