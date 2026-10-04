//! squigl-cli: an Android phone's camera as a V4L2 webcam (`/dev/videoN`). V4L2 is
//! Linux's, so the CLI is too; elsewhere it builds (a workspace build works on every
//! OS) and says so when run. The window, `squigl`, runs everywhere.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod webrtc_server;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "squigl-cli exposes the phone as a V4L2 webcam, which only Linux has. \
         Use the window, squigl, on this system."
    );
    std::process::exit(2);
}
