use super::*;
use std::io::Write;
use std::net::{Ipv4Addr, TcpStream};
use std::sync::mpsc;

const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// The server over plain HTTP (the TLS layer is tiny_http's), on any port.
fn server(on_session: OnSession) -> PairingServer {
    let server = Server::http((LOCAL, 0)).unwrap();
    PairingServer::serve(server, "http", LOCAL, Backend::Openh264, on_session)
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
    let svg = pairing.qr_svg();
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

/// A phone's browser, played by str0m, pairs over `/whip`: the session it is
/// handed must decode its frames.
#[test]
fn a_paired_browser_streams_frames() {
    use crate::testing::FakePhone;
    use squigl_core::decode::YuvFrame;

    let (frames_tx, frames) = mpsc::channel::<(usize, usize)>();
    let stop = Arc::new(AtomicBool::new(false));
    let pairing = server(Box::new({
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
    }));
    let url = pairing.url().to_string();
    let _phone = FakePhone::connect((320, 240), |offer| {
        match request(&url, "POST", &format!("/whip?t={}", token_of(&url)), offer) {
            (201, answer) => Ok(answer),
            (status, body) => Err(format!("{status}: {body}")),
        }
    })
    .unwrap();
    let got = frames
        .recv_timeout(Duration::from_secs(20))
        .expect("a frame");
    stop.store(true, Ordering::Relaxed);
    assert_eq!(got, (320, 240));
}
