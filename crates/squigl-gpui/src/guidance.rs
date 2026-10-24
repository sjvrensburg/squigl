//! Plain-language help for each reason the picture is not coming
//! (`squigl_engine::stream::Problem`), ported from the Tauri UI's
//! `guidance.ts`. Short steps, in the order to try them. Linux wording: this
//! front end is Linux-only.

use squigl_engine::stream::Problem;

pub struct Guidance {
    pub title: &'static str,
    pub steps: Vec<String>,
}

const ENABLE_DEBUGGING: [&str; 2] = [
    "On the phone, open Settings, then About phone, and tap Build number seven times to turn on Developer options.",
    "In Developer options (under Settings, then System), turn on USB debugging.",
];

pub fn guidance(problem: Problem) -> Guidance {
    match problem {
        Problem::AdbMissing => Guidance {
            title: "Squigl needs Android's adb tool",
            steps: vec![
                "Install it with your package manager: the package is called android-tools (Fedora, Arch) or adb (Ubuntu, Debian).".into(),
                "Then start Squigl again.".into(),
            ],
        },
        Problem::NoPhone => Guidance {
            title: "No phone found",
            steps: std::iter::once(
                "Connect the phone to this computer with a USB cable that carries data (some only charge).".to_string(),
            )
            .chain(ENABLE_DEBUGGING.iter().map(ToString::to_string))
            .chain(std::iter::once(
                "When the phone asks whether to allow USB debugging, tap Allow.".to_string(),
            ))
            .collect(),
        },
        Problem::Unauthorized => Guidance {
            title: "Look at your phone",
            steps: vec![
                "Unlock the phone. It is asking whether to allow USB debugging from this computer.".into(),
                "Tick \u{201c}Always allow from this computer\u{201d}, then tap Allow.".into(),
                "No question on the screen? Unplug the cable and plug it in again.".into(),
            ],
        },
        Problem::Offline => Guidance {
            title: "The phone is not answering",
            steps: vec![
                "Unplug the cable and plug it in again.".into(),
                "If that does not help, turn USB debugging off and on again in Developer options.".into(),
            ],
        },
        Problem::SeveralPhones => Guidance {
            title: "More than one phone is connected",
            steps: vec!["Unplug every phone but the one to use.".into()],
        },
        Problem::CameraInUse => Guidance {
            title: "Another app is using the phone's camera",
            steps: vec![
                "Close the camera app on the phone, and any other app that might be using the camera (a video call, a scanner).".into(),
                "Then press Try now.".into(),
            ],
        },
        Problem::AndroidTooOld => Guidance {
            title: "This phone's Android is too old for its camera to be used",
            steps: vec![
                "Squigl needs Android 12 or newer on the phone.".into(),
                "You can still open pictures and scans with Open image.".into(),
            ],
        },
        Problem::FileMissing => Guidance {
            title: "The file could not be opened",
            steps: vec!["Check that it is still there, then open it again, or use the phone.".into()],
        },
        Problem::Other => Guidance {
            title: "The picture stopped",
            steps: vec!["Check the phone and its cable. Here is what went wrong:".into()],
        },
    }
}
