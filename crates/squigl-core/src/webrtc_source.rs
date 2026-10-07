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
//! H.264 only ([`WebrtcSource::accept_offer`] restricts the SDP negotiation to it), so
//! the browser page must prefer/force H.264 in its `getUserMedia`/transceiver setup:
//! str0m's H.264 depacketizer hands back complete Annex-B access units (start-code
//! delimited, exactly what [`crate::decode::Decoder::decode`] already expects from the
//! scrcpy protocol path), so no format conversion is needed between the two sources.
//!
//! Camera controls travel on a data channel the page opens, labelled
//! [`CONTROL_CHANNEL`], as JSON text: the page reports its camera as a
//! [`RemoteCamera`] when the channel opens and after every change, and takes
//! `{"zoom": ratio}` and `{"torch": on}` (`MediaStreamTrack.applyConstraints`). A
//! page without the channel, or a camera without either control, simply has none
//! ([`WebrtcControl::camera`]).

use crate::decode::{self, Decoder, YuvFrame};
use crate::error::{Error, Result};
use crate::session::STALL_TIMEOUT;
use crate::sink::FrameSink;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

/// Max UDP datagram str0m expects to handle (matches its own internal ceiling).
const RECV_BUF: usize = 2000;

/// The label of the data channel camera controls travel on.
pub const CONTROL_CHANNEL: &str = "squigl-control";

/// The longest the session loop waits on the socket, so a control asked for from
/// another thread goes out within about this long.
const CONTROL_LATENCY: Duration = Duration::from_millis(50);

/// What the phone's page says about its camera (`MediaStreamTrack.getCapabilities`
/// and `getSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RemoteCamera {
    /// The zoom's range, if the camera has one.
    pub zoom_range: Option<(f32, f32)>,
    /// The zoom it is at.
    #[serde(default = "one")]
    pub zoom: f32,
    /// Whether the torch is on, if the camera has one.
    pub torch: Option<bool>,
}

fn one() -> f32 {
    1.0
}

/// A session's camera controls, usable from any thread while [`WebrtcSource::run`]
/// blocks (as [`crate::CameraControl`] is for the ADB session). Asking for a zoom
/// or torch queues it; only the newest of each goes out.
#[derive(Clone, Default)]
pub struct WebrtcControl(Arc<Mutex<ControlState>>);

#[derive(Default)]
struct ControlState {
    camera: Option<RemoteCamera>,
    zoom: Option<f32>,
    torch: Option<bool>,
    /// What is still to be sent.
    send_zoom: Option<f32>,
    send_torch: Option<bool>,
    /// Messages sent and not yet answered (the page reports once per message).
    unanswered: usize,
}

impl WebrtcControl {
    /// The camera as the phone last reported it; `None` until it has (or if its
    /// page has no control channel).
    pub fn camera(&self) -> Option<RemoteCamera> {
        self.0.lock().unwrap().camera
    }

    /// The zoom last asked for, else the one reported.
    pub fn zoom(&self) -> Option<f32> {
        let state = self.0.lock().unwrap();
        state.zoom.or(state.camera.map(|c| c.zoom))
    }

    /// The torch last asked for, else the one reported.
    pub fn torch(&self) -> Option<bool> {
        let state = self.0.lock().unwrap();
        state.torch.or(state.camera.and_then(|c| c.torch))
    }

    /// Asks for the zoom `ratio`, clamped to the camera's range. Returns the zoom
    /// asked for, or `None` if the camera has no zoom.
    pub fn set_zoom(&self, ratio: f32) -> Option<f32> {
        let mut state = self.0.lock().unwrap();
        let (min, max) = state.camera?.zoom_range?;
        let ratio = ratio.clamp(min, max);
        state.zoom = Some(ratio);
        state.send_zoom = Some(ratio);
        Some(ratio)
    }

    /// Turns the torch on or off. Returns whether the camera has one.
    pub fn set_torch(&self, on: bool) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.camera.and_then(|c| c.torch).is_none() {
            return false;
        }
        state.torch = Some(on);
        state.send_torch = Some(on);
        true
    }

    /// Takes in a report from the page. Once the page has answered everything
    /// asked of it, its own word stands (until then, what was asked for: a report
    /// in between would jump a slider back).
    fn reported(&self, text: &str) {
        match serde_json::from_str::<RemoteCamera>(text) {
            Ok(camera) => {
                let mut state = self.0.lock().unwrap();
                state.unanswered = state.unanswered.saturating_sub(1);
                if state.unanswered == 0 && state.send_zoom.is_none() && state.send_torch.is_none()
                {
                    state.zoom = None;
                    state.torch = None;
                }
                state.camera = Some(camera);
            }
            Err(e) => log::debug!("ignoring a control message ({e}): {text}"),
        }
    }

    /// The messages waiting to go to the page.
    fn outgoing(&self) -> Vec<String> {
        let mut state = self.0.lock().unwrap();
        let zoom = state.send_zoom.take().map(|z| format!(r#"{{"zoom":{z}}}"#));
        let torch = state
            .send_torch
            .take()
            .map(|t| format!(r#"{{"torch":{t}}}"#));
        let messages: Vec<String> = zoom.into_iter().chain(torch).collect();
        state.unanswered += messages.len();
        messages
    }

    /// A message that never went out: no answer is coming for it.
    fn answered(&self) {
        let mut state = self.0.lock().unwrap();
        state.unanswered = state.unanswered.saturating_sub(1);
    }

    /// Whether anything waits to be sent.
    fn pending(&self) -> bool {
        let state = self.0.lock().unwrap();
        state.send_zoom.is_some() || state.send_torch.is_some()
    }
}

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
    control: WebrtcControl,
    /// The control channel, once the page has opened it.
    channel: Option<ChannelId>,
}

impl WebrtcSource {
    /// Accepts a browser's SDP offer (the WHIP POST body), binds a UDP socket on
    /// `bind_addr` for the media itself, and returns the session plus the SDP answer
    /// to send back (the WHIP response body). Media only starts flowing once the
    /// caller drives [`Self::run`] -- ICE and DTLS complete there, not here.
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
                control: WebrtcControl::default(),
                channel: None,
            },
            answer.to_sdp_string(),
        ))
    }

    /// The session's camera controls (zoom and torch, when the page reports them).
    pub fn control(&self) -> WebrtcControl {
        self.control.clone()
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

    /// Drives str0m (UDP I/O, ICE, DTLS, RTP reassembly) until one frame decodes,
    /// `stop` is raised, or the peer disconnects.
    fn next_frame(&mut self, stop: &AtomicBool) -> Result<Option<YuvFrame>> {
        let mut buf = [0u8; RECV_BUF];
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if let Some(id) = self.channel.filter(|_| self.control.pending()) {
                for message in self.control.outgoing() {
                    let written = self
                        .rtc
                        .channel(id)
                        .map(|mut c| c.write(false, message.as_bytes()));
                    if !matches!(written, Some(Ok(true))) {
                        log::warn!("could not send {message} to the phone");
                        self.control.answered();
                    }
                }
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
                Output::Event(Event::ChannelOpen(id, label)) => {
                    log::debug!("data channel {label:?} open");
                    if label == CONTROL_CHANNEL {
                        self.channel = Some(id);
                    }
                    continue;
                }
                Output::Event(Event::ChannelData(data)) => {
                    log::debug!(
                        "control message on {:?}: {} bytes",
                        data.id,
                        data.data.len()
                    );
                    if Some(data.id) == self.channel && !data.binary {
                        self.control.reported(&String::from_utf8_lossy(&data.data));
                    }
                    continue;
                }
                Output::Event(Event::ChannelClose(id)) => {
                    if Some(id) == self.channel {
                        self.channel = None;
                    }
                    continue;
                }
                Output::Event(_) => continue,
            };

            let wait = timeout
                .saturating_duration_since(Instant::now())
                .clamp(Duration::from_millis(1), CONTROL_LATENCY);
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

#[cfg(test)]
mod tests {
    use super::*;

    const CAMERA: &str = r#"{"zoom_range":[1,8],"zoom":1,"torch":false}"#;

    #[test]
    fn nothing_is_asked_of_a_camera_that_has_not_reported() {
        let control = WebrtcControl::default();
        assert_eq!(control.set_zoom(2.0), None);
        assert!(!control.set_torch(true));
        assert!(control.outgoing().is_empty());
        // A camera with neither control.
        control.reported(r#"{"zoom_range":null,"zoom":1,"torch":null}"#);
        assert_eq!(control.set_zoom(2.0), None);
        assert!(!control.set_torch(true));
    }

    #[test]
    fn only_the_newest_ask_goes_out_and_is_shown_until_answered() {
        let control = WebrtcControl::default();
        control.reported(CAMERA);
        assert_eq!(control.set_zoom(2.0), Some(2.0));
        assert_eq!(control.set_zoom(20.0), Some(8.0), "clamped to the range");
        assert!(control.set_torch(true));
        assert_eq!(control.outgoing(), [r#"{"zoom":8}"#, r#"{"torch":true}"#]);
        assert!(!control.pending());
        // A report from before the page took them in does not move what is shown.
        control.reported(CAMERA);
        assert_eq!((control.zoom(), control.torch()), (Some(8.0), Some(true)));
        // Once both are answered the phone's word stands, even where it differs.
        control.reported(r#"{"zoom_range":[1,8],"zoom":7.5,"torch":true}"#);
        assert_eq!((control.zoom(), control.torch()), (Some(7.5), Some(true)));
        assert_eq!(control.camera().unwrap().zoom_range, Some((1.0, 8.0)));
    }

    #[test]
    fn a_report_may_leave_out_the_zoom() {
        let control = WebrtcControl::default();
        control.reported(r#"{"zoom_range":null,"torch":true}"#);
        assert_eq!(control.zoom(), Some(1.0));
        control.reported("not json");
        assert_eq!(control.torch(), Some(true));
    }
}
