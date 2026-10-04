import { mount } from "svelte";
import { invoke } from "@tauri-apps/api/core";

// The page's errors reach the app's log.
const report = (level: string, message: unknown) =>
  invoke("page_log", { level, message: String(message) }).catch(() => {});
addEventListener("error", (e) => report("error", `${e.message} (${e.filename}:${e.lineno})`));
addEventListener("unhandledrejection", (e) => report("error", e.reason));
report("info", `page loaded: ${navigator.userAgent}`);
const warn = console.warn;
console.warn = (...args: unknown[]) => {
  report("warn", args.join(" "));
  warn(...args);
};
const error = console.error;
console.error = (...args: unknown[]) => {
  report("error", args.join(" "));
  error(...args);
};
import App from "./App.svelte";

export default mount(App, { target: document.getElementById("app")! });
