// A minimal W3C WebDriver client for the end-to-end tests, talking to tauri-driver
// (which starts the app in place of a browser). No dependencies: Node's fetch.
//
// Environment:
//   SQUIGL_DESKTOP    the app (default: target/release/squigl-desktop[.exe])
//   TAURI_DRIVER      tauri-driver (default: from PATH)
//   NATIVE_DRIVER     the platform's driver, if not on PATH (WebKitWebDriver on
//                     Linux, msedgedriver on Windows)
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, openSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "../../..");
const PORT = 4444;
const EXE = process.platform === "win32" ? ".exe" : "";

export const APP =
  process.env.SQUIGL_DESKTOP ?? join(ROOT, "target", "release", `squigl-desktop${EXE}`);

/** A scratch directory for one run's files. */
export const SCRATCH = mkdtempSync(join(tmpdir(), "squigl-e2e-"));

export const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));

/** Polls `check` until it returns something truthy; fails with `what` after `ms`. */
export async function until(check, what, ms = 15000) {
  const deadline = Date.now() + ms;
  let last;
  for (;;) {
    try {
      last = await check();
      if (last) return last;
    } catch (e) {
      last = e;
    }
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what} (last: ${last})`);
    await sleep(100);
  }
}

/** Starts tauri-driver; `stop()` it when done. */
export async function startDriver() {
  const args = ["--port", String(PORT)];
  if (process.env.NATIVE_DRIVER) args.push("--native-driver", process.env.NATIVE_DRIVER);
  // The app's log, wherever its console is (none for a Windows release build).
  const env = { ...process.env, SQUIGL_LOG_FILE: process.env.SQUIGL_LOG_FILE ?? join(SCRATCH, "app.log") };
  // The drivers' output goes to a file, not this process's pipes: a driver or app
  // left running would hold them open and the test runner would wait on it for
  // ever. In a group of its own (POSIX), so stopping it stops what it started.
  const log = openSync(process.env.SQUIGL_DRIVER_LOG ?? join(SCRATCH, "driver.log"), "a");
  const child = spawn(process.env.TAURI_DRIVER ?? "tauri-driver", args, {
    stdio: ["ignore", log, log],
    env,
    detached: process.platform !== "win32",
  });
  let exited = null;
  child.on("exit", (code) => (exited = code));
  await until(async () => {
    if (exited !== null) throw new Error(`tauri-driver exited (${exited})`);
    const r = await fetch(`http://127.0.0.1:${PORT}/status`);
    return r.ok;
  }, "tauri-driver to listen");
  return {
    stop() {
      if (exited !== null) return;
      if (process.platform === "win32") {
        spawnSync("taskkill", ["/pid", String(child.pid), "/t", "/f"], { stdio: "ignore" });
      } else {
        try {
          process.kill(-child.pid, "SIGTERM");
        } catch {
          child.kill();
        }
      }
    },
  };
}

/**
 * `--name value` pairs as `--name=value`: msedgedriver passes the app's arguments
 * as Chromium switches, putting `--` before (and lowercasing) any that lacks it, so
 * a separate value would arrive as a switch of its own.
 */
function joined(args) {
  const out = [];
  for (let i = 0; i < args.length; i++) {
    const next = args[i + 1];
    if (args[i].startsWith("--") && next !== undefined && !next.startsWith("--")) {
      out.push(`${args[i]}=${next}`);
      i++;
    } else {
      out.push(args[i]);
    }
  }
  return out;
}

export const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";

/** Keys WebDriver spells as private-use characters. */
export const KEY = {
  Escape: "",
  Tab: "",
  Shift: "",
  ArrowLeft: "",
  ArrowUp: "",
  ArrowRight: "",
  ArrowDown: "",
};

export class Session {
  constructor(id) {
    this.id = id;
  }

  /** Launches the app with `args` (each run gets its own settings file). */
  static async start(args) {
    const config = join(SCRATCH, `config-${Date.now()}-${Math.random().toString(36).slice(2)}.toml`);
    const body = {
      capabilities: {
        alwaysMatch: {
          "tauri:options": { application: APP, args: joined([...args, "--dev-config", config]) },
        },
      },
    };
    const value = await call("POST", "/session", body);
    return new Session(value.sessionId);
  }

  cmd(method, path, body) {
    return call(method, `/session/${this.id}${path}`, body);
  }

  /** Runs `script` (a function body) in the page with `args`; awaits a returned promise. */
  execute(script, ...args) {
    return this.cmd("POST", "/execute/sync", { script, args });
  }

  /** `window.squiglProbe[name](...args)`. */
  probe(name, ...args) {
    return this.execute(
      `const p = window.squiglProbe; return p ? p[arguments[0]](...arguments[1]) : null;`,
      name,
      args,
    );
  }

  async find(css) {
    const el = await this.cmd("POST", "/element", { using: "css selector", value: css });
    return el[ELEMENT];
  }

  async findAll(css) {
    const els = await this.cmd("POST", "/elements", { using: "css selector", value: css });
    return els.map((e) => e[ELEMENT]);
  }

  text(el) {
    return this.cmd("GET", `/element/${el}/text`);
  }

  attribute(el, name) {
    return this.cmd("GET", `/element/${el}/attribute/${name}`);
  }

  property(el, name) {
    return this.cmd("GET", `/element/${el}/property/${name}`);
  }

  click(el) {
    return this.cmd("POST", `/element/${el}/click`, {});
  }

  /** Types `text` into the element (focusing it first). */
  type(el, text) {
    return this.cmd("POST", `/element/${el}/value`, { text });
  }

  /** Presses and releases `key` at whatever has focus. */
  async press(key) {
    await this.cmd("POST", "/actions", {
      actions: [
        {
          type: "key",
          id: "keyboard",
          actions: [
            { type: "keyDown", value: key },
            { type: "keyUp", value: key },
          ],
        },
      ],
    });
  }

  quit() {
    return this.cmd("DELETE", "");
  }
}

async function call(method, path, body) {
  const r = await fetch(`http://127.0.0.1:${PORT}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) {
    const v = json.value ?? {};
    throw new Error(`${method} ${path}: ${v.error ?? r.status}: ${v.message ?? ""}`);
  }
  return json.value;
}
