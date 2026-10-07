use super::*;
use std::io::Write;
use std::net::{Ipv4Addr, TcpStream};
use std::sync::mpsc;

const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// The server over plain HTTP (the TLS layer is tiny_http's), on any port.
fn server(on_session: OnSession) -> PairingServer {
    server_at(&[LOCAL], on_session)
}

/// [`server`], listening at each of `ips`.
fn server_at(ips: &[IpAddr], on_session: OnSession) -> PairingServer {
    let token = token();
    let listeners = ips
        .iter()
        .map(|&ip| {
            let server = Server::http((ip, 0)).unwrap();
            let port = server.server_addr().to_ip().unwrap().port();
            Listener {
                server,
                bind: ip,
                offer: Offer {
                    network: Network::Local,
                    url: format!("http://{}:{port}/?t={token}", host(ip)),
                    trusted: false,
                },
            }
        })
        .collect();
    PairingServer::serve(listeners, token, Backend::Openh264, on_session)
}

fn refuse() -> OnSession {
    Box::new(|_| Err("not now".to_string()))
}

/// One request; the status code and body.
fn request(url: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let addr = url.trim_start_matches("http://").split('/').next().unwrap();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    let status = reply[9..12].parse().unwrap();
    let body = reply.split_once("\r\n\r\n").unwrap().1.to_string();
    (status, body)
}

fn token_of(url: &str) -> &str {
    url.split_once("?t=").unwrap().1
}

#[test]
fn the_page_and_whip_answer_only_with_the_token() {
    let pairing = server(refuse());
    let url = pairing.url().to_string();
    let token = token_of(&url);
    assert_eq!(token.len(), 32);

    let (status, page) = request(&url, "GET", &format!("/?t={token}"), "");
    assert_eq!(status, 200);
    assert!(page.contains("getUserMedia"));

    assert_eq!(request(&url, "GET", "/", "").0, 403);
    assert_eq!(request(&url, "GET", "/?t=0123", "").0, 403);
    assert_eq!(request(&url, "POST", "/whip", "v=0").0, 403);
    assert_eq!(request(&url, "POST", "/whip?t=nope", "v=0").0, 403);
    // With the token, a bad offer is the problem.
    assert_eq!(
        request(&url, "POST", &format!("/whip?t={token}"), "v=0").0,
        400
    );
    assert_eq!(
        request(&url, "GET", &format!("/other?t={token}"), "").0,
        404
    );
}

#[test]
fn each_start_has_its_own_token() {
    let (a, b) = (server(refuse()), server(refuse()));
    assert_ne!(token_of(a.url()), token_of(b.url()));
}

#[test]
fn the_qr_code_is_an_svg_of_the_address() {
    let pairing = server(refuse());
    let svg = qr_svg(pairing.url());
    assert!(svg.contains("<svg"), "{svg}");
    assert!(svg.contains("#000000") && svg.contains("#ffffff"));
}

#[test]
fn the_certificate_is_kept_for_the_same_address() {
    let dir = std::env::temp_dir().join(format!("squigl-pairing-{}", token()));
    let names = |ip: &str| vec![ip.to_string()];
    let first = certificate(Some(&dir), &names("192.168.1.20")).unwrap();
    let again = certificate(Some(&dir), &names("192.168.1.20")).unwrap();
    assert_eq!(first.cert_pem, again.cert_pem);
    assert_eq!(first.key_pem, again.key_pem);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("pairing.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the key is readable by others: {mode:o}");
    }
    let moved = certificate(Some(&dir), &names("10.0.0.5")).unwrap();
    assert_ne!(first.cert_pem, moved.cert_pem);
    // Without a directory, every start makes its own.
    let (a, b) = (
        certificate(None, &names("10.0.0.5")).unwrap(),
        certificate(None, &names("10.0.0.5")).unwrap(),
    );
    assert_ne!(a.cert_pem, b.cert_pem);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A server at `ips` whose sessions run, sending each frame's size to the
/// receiver until the flag is raised.
fn streaming_server(
    ips: &[IpAddr],
) -> (
    PairingServer,
    mpsc::Receiver<(usize, usize)>,
    Arc<AtomicBool>,
) {
    use squigl_core::decode::YuvFrame;
    let (frames_tx, frames) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let pairing = server_at(
        ips,
        Box::new({
            let stop = Arc::clone(&stop);
            move |mut session: WebrtcSource| {
                let (frames_tx, stop) = (frames_tx.clone(), Arc::clone(&stop));
                std::thread::spawn(move || {
                    let mut sink = |frame: &YuvFrame| {
                        let _ = frames_tx.send((frame.width, frame.height));
                        Ok(())
                    };
                    let _ = session.run(&mut sink, &stop);
                });
                Ok(())
            }
        }),
    );
    (pairing, frames, stop)
}

/// A phone's browser, played by str0m, pairing over `/whip` at `url`.
fn phone_at(url: &str, size: (usize, usize)) -> crate::testing::FakePhone {
    crate::testing::FakePhone::connect(size, |offer| {
        match request(url, "POST", &format!("/whip?t={}", token_of(url)), offer) {
            (201, answer) => Ok(answer),
            (status, body) => Err(format!("{status}: {body}")),
        }
    })
    .unwrap()
}

/// The session a paired browser is handed must decode its frames.
#[test]
fn a_paired_browser_streams_frames() {
    let (pairing, frames, stop) = streaming_server(&[LOCAL]);
    let _phone = phone_at(pairing.url(), (320, 240));
    let got = frames
        .recv_timeout(Duration::from_secs(20))
        .expect("a frame");
    stop.store(true, Ordering::Relaxed);
    assert_eq!(got, (320, 240));
}

/// Each address is its own listener, and the media goes over the one the phone
/// came in at (127.0.0.2: all of 127/8 is loopback on Linux).
#[cfg(target_os = "linux")]
#[test]
fn a_phone_pairs_at_any_offered_address() {
    let second: IpAddr = "127.0.0.2".parse().unwrap();
    let (pairing, frames, stop) = streaming_server(&[LOCAL, second]);
    let urls: Vec<&str> = pairing.offers().iter().map(|o| o.url.as_str()).collect();
    assert_eq!(urls.len(), 2);
    assert!(urls[1].starts_with("http://127.0.0.2:"), "{urls:?}");
    assert_eq!(
        token_of(urls[0]),
        token_of(urls[1]),
        "one token for the start"
    );
    let token = token_of(urls[1]);
    assert_eq!(request(urls[1], "GET", &format!("/?t={token}"), "").0, 200);
    let _phone = phone_at(urls[1], (160, 120));
    let got = frames
        .recv_timeout(Duration::from_secs(20))
        .expect("a frame");
    stop.store(true, Ordering::Relaxed);
    assert_eq!(got, (160, 120));
}

#[test]
fn a_start_offers_every_address_it_could_listen_at() {
    let start = |ips: &[&str]| {
        PairingServer::start(
            PairingOptions {
                addresses: ips
                    .iter()
                    .map(|ip| Address::given(ip.parse().unwrap()))
                    .collect(),
                port: 0,
                decoder: Backend::Openh264,
                cert_dir: None,
                tailscale_https: false,
            },
            |_| Ok(()),
        )
    };
    // 192.0.2.1 (documentation space) is nobody's: it cannot be listened at.
    let pairing = start(&["192.0.2.1", "127.0.0.1"]).unwrap();
    assert_eq!(pairing.offers().len(), 1);
    assert!(
        pairing.url().starts_with("https://127.0.0.1:"),
        "{}",
        pairing.url()
    );
    assert!(!pairing.offers()[0].trusted);
    assert!(start(&["192.0.2.1"]).is_err());
    assert!(start(&[]).is_err());
}

/// Waits up to 20 s for `done`.
fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The phone reports its camera on the control channel, and zoom and torch reach
/// it while the session streams.
#[test]
fn zoom_and_torch_reach_the_phone() {
    use squigl_core::decode::YuvFrame;
    let (tx, rx) = mpsc::channel();
    let phone = crate::testing::FakePhone::connect((160, 120), |offer| {
        let (session, answer) = WebrtcSource::accept_offer(offer, LOCAL, Backend::Openh264)
            .map_err(|e| e.to_string())?;
        tx.send(session).unwrap();
        Ok(answer)
    })
    .unwrap();
    let mut session = rx.recv().unwrap();
    let control = session.control();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::spawn({
        let stop = Arc::clone(&stop);
        move || session.run(&mut |_: &YuvFrame| Ok(()), &stop)
    });
    eventually("the phone's report", || control.camera().is_some());
    assert_eq!(control.camera(), Some(crate::testing::CAMERA));
    assert_eq!(control.set_zoom(3.0), Some(3.0));
    assert!(control.set_torch(true));
    eventually("the zoom and torch", || {
        let camera = phone.camera();
        camera.zoom == 3.0 && camera.torch == Some(true)
    });
    // The phone's answer becomes what is shown.
    eventually("the answer", || control.camera().unwrap().zoom == 3.0);
    assert_eq!((control.zoom(), control.torch()), (Some(3.0), Some(true)));
    stop.store(true, Ordering::Relaxed);
    thread.join().unwrap().unwrap();
}
