// Plain-language help for each reason the picture is not coming
// (squigl_engine::stream::Problem). Short steps, in the order to try them.
import type { Problem } from "./engine";

export type Platform = "linux" | "windows" | "mac";

export interface Guidance {
  title: string;
  steps: string[];
}

const ENABLE_DEBUGGING = [
  "On the phone, open Settings, then About phone, and tap Build number seven times to turn on Developer options.",
  "In Developer options (under Settings, then System), turn on USB debugging.",
];

export function guidance(problem: Problem, platform: Platform): Guidance {
  switch (problem) {
    case "adb-missing":
      return {
        title: "Squigl needs Android's adb tool",
        steps: [
          platform === "linux"
            ? "Install it with your package manager: the package is called android-tools (Fedora, Arch) or adb (Ubuntu, Debian)."
            : "Download Android's Platform-Tools from developer.android.com/tools/releases/platform-tools and unzip them.",
          platform === "linux"
            ? "Then start Squigl again."
            : "Put adb next to Squigl, or set the SQUIGL_ADB environment variable to where adb is, then start Squigl again.",
        ],
      };
    case "no-phone":
      return {
        title: "No phone found",
        steps: [
          "Connect the phone to this computer with a USB cable that carries data (some only charge).",
          ...ENABLE_DEBUGGING,
          "When the phone asks whether to allow USB debugging, tap Allow.",
        ],
      };
    case "unauthorized":
      return {
        title: "Look at your phone",
        steps: [
          "Unlock the phone. It is asking whether to allow USB debugging from this computer.",
          "Tick “Always allow from this computer”, then tap Allow.",
          "No question on the screen? Unplug the cable and plug it in again.",
        ],
      };
    case "offline":
      return {
        title: "The phone is not answering",
        steps: [
          "Unplug the cable and plug it in again.",
          "If that does not help, turn USB debugging off and on again in Developer options.",
        ],
      };
    case "several-phones":
      return {
        title: "More than one phone is connected",
        steps: ["Unplug every phone but the one to use."],
      };
    case "camera-in-use":
      return {
        title: "Another app is using the phone's camera",
        steps: [
          "Close the camera app on the phone, and any other app that might be using the camera (a video call, a scanner).",
          "Then press Try now.",
        ],
      };
    case "android-too-old":
      return {
        title: "This phone's Android is too old for its camera to be used",
        steps: [
          "Squigl needs Android 12 or newer on the phone.",
          "You can still open pictures and scans with Open image.",
        ],
      };
    case "file-missing":
      return {
        title: "The file could not be opened",
        steps: ["Check that it is still there, then open it again, or use the phone."],
      };
    case "other":
      return {
        title: "The picture stopped",
        steps: ["Check the phone and its cable. Here is what went wrong:"],
      };
  }
}
