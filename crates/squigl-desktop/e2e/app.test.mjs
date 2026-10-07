// End-to-end tests of the desktop app through tauri-driver: the magnifier on a
// replayed recording (no phone), driven from the keyboard, and each display mode's
// drawn pixels against the engine's reference (Engine::displayed_pixel).
//
//   cargo build --release -p squigl-desktop
//   node --test crates/squigl-desktop/e2e/app.test.mjs
//
// Needs tauri-driver and the platform's WebDriver (see driver.mjs); macOS has none.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import http from "node:http";
import https from "node:https";
import { join, resolve } from "node:path";
import { after, before, describe, test } from "node:test";
import { ELEMENT, KEY, SCRATCH, Session, sleep, startDriver, until } from "./driver.mjs";
import { writePng } from "./png.mjs";

const ROOT = resolve(import.meta.dirname, "../../..");

let driver;
before(async () => {
  driver = await startDriver();
});
after(() => driver?.stop());

/** The status at the footer's left: Live, Frozen, Connecting… */
async function status(s) {
  return s.text(await s.find("footer span"));
}

async function notice(s) {
  return s.text(await s.find("footer [role=status]"));
}

async function canvas(s) {
  return s.find("canvas");
}

/**
 * GETs `url` over HTTPS on a connection of its own (a kept-alive one would outlive
 * the server), taking any certificate: the pairing server's is self-signed.
 */
function get(url) {
  return new Promise((ok, fail) => {
    https
      .get(url, { rejectUnauthorized: false, agent: false, timeout: 5000 }, (r) => {
        let body = "";
        r.on("data", (chunk) => (body += chunk));
        r.on("end", () => ok({ status: r.statusCode, body }));
      })
      .on("timeout", function () {
        this.destroy(new Error(`no answer from ${url}`));
      })
      .on("error", fail);
  });
}

describe("the magnifier on a recording", () => {
  let s;
  before(async () => {
    const recording = join(SCRATCH, "bars.sqrec");
    execFileSync(
      "cargo",
      ["run", "-q", "--release", "-p", "squigl-core", "--example", "synth_recording", "--"]
        .concat([recording, "640x480", "2"]),
      { cwd: ROOT, stdio: "inherit" },
    );
    // Two addresses to pair at, as a LAN and an overlay would give (all of 127/8
    // is loopback on Linux and Windows).
    s = await Session.start([
      "--replay", recording, "--dev-probe",
      "--dev-pairing-bind", "127.0.0.1,127.0.0.2",
    ]);
    await until(() => s.probe("drawn"), "the first frame", 30000);
  });
  after(() => s?.quit());

  test("plays live", async () => {
    await until(async () => (await status(s)) === "Live", "Live");
    const header = await s.probe("header");
    assert.deepEqual(header.view, [640, 480]);
    const before = await s.probe("drawn");
    await until(async () => (await s.probe("drawn")) > before + 5, "more frames");
  });

  test("freezes and goes live from the keyboard", async () => {
    await s.type(await canvas(s), " ");
    await until(async () => (await status(s)) === "Frozen", "Frozen");
    const button = await s.find("header button");
    assert.equal(await s.attribute(button, "aria-pressed"), "true");
    await sleep(300); // a request already on its way may still land
    const frozen = await s.probe("drawn");
    await sleep(1000);
    assert.equal(await s.probe("drawn"), frozen, "nothing is drawn while frozen");
    await s.type(await canvas(s), " ");
    await until(async () => (await status(s)) === "Live", "Live again");
    await until(async () => (await s.probe("drawn")) > frozen + 5, "frames again");
  });

  test("rotates", async () => {
    await s.type(await canvas(s), "r");
    await until(async () => (await s.probe("header"))?.view[0] === 480, "a turned view");
    assert.equal(await notice(s), "Turned right");
    await s.type(await canvas(s), "R");
    await until(async () => (await s.probe("header"))?.view[0] === 640, "the view turned back");
  });

  test("magnifies, pans and resets", async () => {
    const output = await s.find("header output");
    assert.equal(await s.text(output), "1.0×");
    // Magnified until the region is cropped both ways (in a wide window the
    // height is cropped first), so a move right has room.
    let zoomed = null;
    for (let i = 0; i < 12 && !zoomed; i++) {
      await s.type(await canvas(s), "+");
      await sleep(200);
      const h = await s.probe("header");
      if (h.view_region.w < 640 && h.view_region.h < 480) zoomed = h;
    }
    assert.ok(zoomed, "the region shrinks as the magnification grows");
    assert.notEqual(await s.text(output), "1.0×");
    await s.type(await canvas(s), KEY.ArrowRight);
    await until(
      async () => (await s.probe("header")).view_region.x > zoomed.view_region.x,
      "the region moved right",
    );
    await s.type(await canvas(s), "0");
    await until(async () => (await s.text(output)) === "1.0×", "back to 1×");
    await until(async () => (await s.probe("header")).view_region.w === 640, "the whole view");
  });

  test("cycles the display mode", async () => {
    const select = await s.find("header select");
    assert.equal(await s.property(select, "value"), "normal");
    await s.type(await canvas(s), "m");
    await until(async () => (await s.property(select, "value")) === "grey", "the next mode");
    assert.equal(await notice(s), "Black on white");
    await s.type(await canvas(s), "m");
    await until(async () => (await s.property(select, "value")) === "inverted", "the next mode");
  });

  test("opens and closes the settings from the keyboard", async () => {
    await s.type(await canvas(s), ",");
    await until(async () => (await s.findAll("dialog[open]")).length === 1, "the dialog");
    await s.press(KEY.Escape);
    await until(async () => (await s.findAll("dialog[open]")).length === 0, "the dialog closed");
  });

  test("Pair phone shows a code and an address serving the phone's page while open", async () => {
    const button = await s.execute(
      `return [...document.querySelectorAll("header button")].find((b) => b.textContent.includes("Pair phone"));`,
    );
    // From the keyboard: WebKitWebDriver's click misses a button on the toolbar's
    // second row.
    await s.execute(`arguments[0].focus();`, button);
    await s.press("\uE007"); // Enter
    const code = await until(async () => (await s.findAll("dialog[open] code"))[0], "the address");
    const url = await s.text(code);
    assert.match(url, /^https:\/\/127\.0\.0\.1:\d+\/\?t=[0-9a-f]{32}$/);
    assert.equal((await s.findAll("dialog[open] .qr svg")).length, 1);
    const page = await get(url);
    assert.equal(page.status, 200);
    assert.match(page.body, /getUserMedia/);
    assert.equal((await get(url.replace(/t=.*/, "t=0"))).status, 403);
    // The other address: its own code and listener, the same token.
    // Chosen from the keyboard, as the radio group goes.
    const radios = await s.findAll("dialog[open] input[type=radio]");
    assert.equal(radios.length, 2);
    await s.execute(`arguments[0].focus();`, { [ELEMENT]: radios[0] });
    await s.press(KEY.ArrowRight);
    const other = await until(async () => {
      const shown = await s.text(await s.find("dialog[open] code"));
      return shown !== url && shown;
    }, "the second address");
    assert.match(other, /^https:\/\/127\.0\.0\.2:\d+\/\?t=/);
    assert.equal(other.split("?t=")[1], url.split("?t=")[1]);
    assert.equal((await get(other)).status, 200);
    await s.press(KEY.Escape);
    await until(async () => (await s.findAll("dialog[open]")).length === 0, "the dialog closed");
    for (const address of [url, other]) {
      await until(
        () => get(address).then(() => false, () => true),
        `the pairing server at ${address} to stop`,
      );
    }
    assert.equal(await status(s), "Live", "the recording still shows");
  });

  test("Tab reaches every toolbar control, and Enter works one", async () => {
    // Number the controls, then walk the focus from the first one.
    const count = await s.execute(`
      const all = [...document.querySelectorAll("header button, header select, header input")];
      all.forEach((el, i) => (el.dataset.e2e = String(i)));
      all[0].focus();
      return all.length;`);
    const reached = new Set();
    for (let i = 0; i < count + 2; i++) {
      reached.add(await s.execute(`return document.activeElement?.dataset.e2e ?? null;`));
      await s.press(KEY.Tab);
    }
    for (let i = 0; i < count; i++) assert.ok(reached.has(String(i)), `control ${i} is reachable`);
    // "Rotate right" is the third button.
    const rotate = (await s.findAll("header button"))[2];
    assert.match(await s.text(rotate), /Rotate right/);
    await s.execute(`arguments[0].focus();`, { [ELEMENT]: rotate });
    await s.press(""); // Enter
    await until(async () => (await s.probe("header"))?.view[0] === 480, "a turned view");
    await s.press("");
    await until(async () => (await s.probe("header"))?.view[0] === 640, "turned again");
  });
});

// Flat patches, eight across and four down: greys, then strong and soft colours.
const PATCHES = [
  [0, 0, 0], [36, 36, 36], [73, 73, 73], [109, 109, 109],
  [146, 146, 146], [182, 182, 182], [219, 219, 219], [255, 255, 255],
  [255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 255, 255],
  [255, 0, 255], [255, 255, 0], [255, 128, 0], [128, 0, 255],
  [128, 64, 64], [64, 128, 64], [64, 64, 128], [200, 160, 120],
  [30, 60, 90], [240, 220, 200], [90, 30, 60], [160, 200, 80],
  [20, 20, 40], [250, 250, 230], [120, 120, 0], [0, 120, 120],
  [180, 40, 40], [40, 180, 40], [40, 40, 180], [128, 128, 128],
];
const MODES = ["normal", "grey", "inverted", "yellow-on-black", "white-on-black", "black-on-yellow", "custom"];
// The shader converts in floating point, the reference in integers.
const TOLERANCE = 3;

/** Every patch centre's drawn colour against its reference: the misses. */
async function misses(s) {
  const points = [];
  for (let i = 0; i < 8; i++) for (let j = 0; j < 8; j++) points.push([(i + 0.5) / 8, (j + 0.5) / 8]);
  const samples = (await s.probe("sample", points)) ?? [null];
  return samples.filter(
    (p) => !p || !p.expected || p.drawn.some((c, i) => Math.abs(c - p.expected[i]) > TOLERANCE),
  );
}

for (const renderer of ["webgl2", "canvas2d"]) {
  describe(`each display mode, drawn with ${renderer}`, () => {
    let s;
    before(async () => {
      const image = join(SCRATCH, "patches.png");
      writePng(image, 320, 160, (x, y) => PATCHES[Math.floor(y / 40) * 8 + Math.floor(x / 40)]);
      const args = ["--open", image, "--dev-probe"];
      if (renderer === "canvas2d") args.push("--dev-canvas2d");
      s = await Session.start(args);
      await until(() => s.probe("drawn"), "the first frame", 30000);
    });
    after(() => s?.quit());

    test("is the renderer asked for", async () => {
      assert.equal(await s.probe("renderer"), renderer);
    });

    for (const turned of [false, true]) {
      test(`matches the engine's colours${turned ? ", turned right" : ""}`, async () => {
        if (turned) {
          await s.type(await canvas(s), "r");
          await until(async () => (await s.probe("header"))?.view[0] === 160, "a turned view");
        }
        for (const mode of MODES) {
          await s.execute(
            `const sel = document.querySelector("header select");
             sel.value = arguments[0];
             sel.dispatchEvent(new Event("change", { bubbles: true }));`,
            mode,
          );
          let last = [];
          try {
            await until(async () => (last = await misses(s)).length === 0, `${mode} to match`, 10000);
          } catch {
            assert.fail(`${mode}: ${last.length} of 64 points differ, e.g. ${JSON.stringify(last.slice(0, 3))}`);
          }
        }
      });
    }
  });
}

describe("at twice the text size", () => {
  let s;
  before(async () => {
    // A small screen as well: CI's Windows runner has 1024x768, which at 2x
    // leaves the page 512 by about 380 CSS pixels.
    s = await Session.start([
      "--test-pattern", "--dev-probe", "--dev-text-scale", "2", "--dev-window-size", "1024x768",
    ]);
    await until(() => s.probe("drawn"), "the first frame", 30000);
  });
  after(() => s?.quit());

  test("the page is zoomed, every control is on screen and the picture keeps its room", async () => {
    const layout = await s.execute(`
      const doc = document.documentElement;
      const inside = (r) => r.left >= 0 && r.top >= 0 && r.right <= innerWidth + 0.5 && r.bottom <= innerHeight + 0.5;
      const controls = [...document.querySelectorAll("header button, header select, header input[type=range], header label.button, footer span")];
      return {
        dpr: devicePixelRatio,
        overflow: doc.scrollWidth > doc.clientWidth || doc.scrollHeight > doc.clientHeight,
        offscreen: controls.filter((el) => !inside(el.getBoundingClientRect())).map((el) => el.textContent.trim()),
        picture: document.querySelector("canvas").getBoundingClientRect().height / innerHeight,
        window: [innerWidth, innerHeight],
      };`);
    assert.ok(layout.dpr >= 2, `zoomed (device pixel ratio ${layout.dpr})`);
    assert.equal(layout.overflow, false, "nothing scrolls");
    assert.deepEqual(layout.offscreen, [], "every control is on screen");
    // The toolbar goes compact in a window this small (short labels, no key hints).
    assert.ok(
      layout.picture >= 1 / 3,
      `the picture has ${Math.round(layout.picture * 100)}% of the height of a ${layout.window.join("x")} window`,
    );
  });
});

describe("each UI theme", () => {
  let s;
  before(async () => {
    s = await Session.start(["--test-pattern", "--dev-probe"]);
    await until(() => s.probe("drawn"), "the first frame", 30000);
  });
  after(() => s?.quit());

  // WCAG 2.1: 7:1 for text in a high-contrast theme, 4.5:1 otherwise; 3:1 for the
  // edges of controls and the focus ring (1.4.11).
  for (const [theme, text] of [
    ["dark", 4.5],
    ["light", 4.5],
    ["high-contrast-yellow", 7],
    ["high-contrast-white", 7],
  ]) {
    test(`${theme} has the contrast it should`, async () => {
      const ratios = await s.execute(
        `const sel = [...document.querySelectorAll("dialog select")].find((s) =>
           [...s.options].some((o) => o.value === "high-contrast-yellow"));
         sel.value = arguments[0];
         sel.dispatchEvent(new Event("change", { bubbles: true }));
         return new Promise((ok) => setTimeout(() => {
           const rgb = (c) => c.match(/[\\d.]+/g).slice(0, 3).map(Number);
           const lum = (c) => {
             const [r, g, b] = rgb(c).map((v) => {
               v /= 255;
               return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
             });
             return 0.2126 * r + 0.7152 * g + 0.0722 * b;
           };
           const ratio = (a, b) => {
             const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p);
             return (x + 0.05) / (y + 0.05);
           };
           // A colour as the page resolves it.
           const probe = document.createElement("i");
           document.body.append(probe);
           const resolve = (v) => {
             probe.style.color = v;
             return getComputedStyle(probe).color;
           };
           const button = getComputedStyle(document.querySelector("header button"));
           const select = getComputedStyle(document.querySelector("header select"));
           const slider = document.querySelector("header input[type=range]");
           const icon = getComputedStyle(document.querySelector("header button svg"));
           const footer = getComputedStyle(document.querySelector("footer"));
           const panel = footer.backgroundColor;
           const out = {
             theme: document.documentElement.dataset.theme,
             button: ratio(button.color, button.backgroundColor),
             // Its own colours only when it is not drawn natively.
             selectDrawn: select.appearance,
             select: ratio(select.color, select.backgroundColor),
             sliderDrawn: getComputedStyle(slider).appearance,
             icon: ratio(icon.stroke, button.backgroundColor),
             footer: ratio(footer.color, panel),
             edge: ratio(button.borderTopColor, panel),
             focus: ratio(resolve("var(--focus)"), panel),
           };
           probe.remove();
           ok(out);
         }, 300));`,
        theme,
      );
      assert.equal(ratios.theme, theme);
      assert.ok(ratios.button >= text, `button text ${ratios.button.toFixed(1)}:1`);
      assert.equal(ratios.selectDrawn, "none", "the select draws its own colours");
      assert.ok(ratios.select >= text, `select text ${ratios.select.toFixed(1)}:1`);
      // Its track is --edge and its thumb --text, both checked against the panel
      // here; WebKit gives no computed style for the slider's parts to read.
      assert.equal(ratios.sliderDrawn, "none", "the slider draws its own colours");
      assert.ok(ratios.icon >= 3, `icons ${ratios.icon.toFixed(1)}:1`);
      assert.ok(ratios.footer >= text, `status text ${ratios.footer.toFixed(1)}:1`);
      assert.ok(ratios.edge >= 3, `control edges ${ratios.edge.toFixed(1)}:1`);
      assert.ok(ratios.focus >= 3, `focus ring ${ratios.focus.toFixed(1)}:1`);
    });
  }
});

describe("a paired phone's camera", () => {
  let s;
  // What the fake phone (--dev-fake-phone: str0m playing the phone's browser) has
  // been asked to do.
  const phone = () => s.execute(`return window.__TAURI_INTERNALS__.invoke("dev_phone_camera");`);
  const torchButton = () =>
    s.execute(
      `return [...document.querySelectorAll("header button")].find((b) => b.textContent.includes("Torch")) ?? null;`,
    );
  before(async () => {
    s = await Session.start(["--test-pattern", "--dev-fake-phone"]);
  });
  after(() => s?.quit());

  test("shows the zoom and torch the phone reports, and both reach it", async () => {
    const torch = await until(torchButton, "the torch button", 30000);
    assert.equal(await status(s), "Live");
    assert.equal(await s.attribute(torch[ELEMENT], "aria-pressed"), "false");
    // From the keyboard: WebKitWebDriver's click does not reach this button.
    await s.execute(`arguments[0].focus();`, torch);
    await s.press("\uE007"); // Enter
    await until(async () => (await phone())?.torch === true, "the torch on at the phone");
    // The page hears of it with the next stream update (4 a second).
    await until(
      async () => (await s.attribute(torch[ELEMENT], "aria-pressed")) === "true",
      "the button pressed",
    );

    // The slider, from the keyboard: End is the camera's longest zoom.
    const zoomLabel = () =>
      s.execute(
        `return [...document.querySelectorAll("header label")].find((l) => l.textContent.includes("Camera zoom")) ?? null;`,
      );
    const label = (await zoomLabel())[ELEMENT];
    await s.execute(`arguments[0].querySelector("input").focus();`, { [ELEMENT]: label });
    await s.press(KEY.End);
    await until(async () => (await phone())?.zoom === 8, "the phone at 8x");
    await until(async () => (await s.text(label)).includes("8.0×"), "8.0× shown");

    // The shortcuts: [ zooms out, t turns the torch off.
    await s.type(await canvas(s), "[");
    await until(async () => (await phone())?.zoom < 8, "the phone zoomed out");
    await s.type(await canvas(s), "t");
    await until(async () => (await phone())?.torch === false, "the torch off at the phone");
  });
});

/**
 * An OpenAI-compatible backend: answers each read with the size of the image it
 * was sent ("seen WxH"), the size a wavering token, so the page's marks show.
 */
function fakeBackend() {
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      const json = JSON.parse(body);
      const url = json.messages[0].content.find((c) => c.type === "image_url").image_url.url;
      const png = Buffer.from(url.split(",")[1], "base64");
      // The PNG's IHDR: width and height at bytes 16 and 20.
      const size = `${png.readUInt32BE(16)}x${png.readUInt32BE(20)}`;
      const tokens = [
        ["seen", 0.99],
        [" ", 0.99],
        [size, 0.7],
      ];
      res.setHeader("content-type", "application/json");
      res.end(
        JSON.stringify({
          choices: [
            {
              message: { role: "assistant", content: `seen ${size}` },
              finish_reason: "stop",
              logprobs: {
                content: tokens.map(([token, p]) => ({
                  token,
                  logprob: Math.log(p),
                  top_logprobs: [{ token: "seem", logprob: Math.log(0.2) }],
                })),
              },
            },
          ],
        }),
      );
    });
  });
  return new Promise((ok) => server.listen(0, "127.0.0.1", () => ok(server)));
}

describe("reading", () => {
  let s;
  let backend;
  const readings = () =>
    s.execute(`return [...document.querySelectorAll(".results .text")].map((p) => p.textContent.trim());`);
  before(async () => {
    backend = await fakeBackend();
    s = await Session.start([
      "--test-pattern",
      "--dev-probe",
      "--dev-backend",
      `http://127.0.0.1:${backend.address().port}/v1`,
    ]);
    await until(() => s.probe("drawn"), "the first frame", 30000);
  });
  after(async () => {
    await s?.quit();
    backend?.close();
  });

  test("Enter reads the page, freezing it, and the unsure words are marked", async () => {
    await s.execute(`document.querySelector("canvas").focus();`);
    await s.press("\uE007"); // Enter
    await until(async () => (await readings()).length === 1, "a reading");
    assert.deepEqual(await readings(), ["seen 640x480"]);
    assert.equal(await status(s), "Frozen", "a read is of a capture");
    const marked = await s.execute(
      `const w = document.querySelector(".results .wavering");
       return w && [w.textContent, w.title];`,
    );
    assert.deepEqual(marked, ["640x480", "or: seem"]);
    assert.match(await s.text(await s.find(".results li")), /1 uncertain word underlined/);
  });

  test("a box drawn on the picture is what is read, and Escape clears it", async () => {
    const overlay = await s.find("svg.overlay");
    await s.drag(overlay, [-120, -60], [80, 40]);
    await until(async () => (await s.findAll("svg.overlay polygon.selection")).length === 1, "the box");
    const label = await until(async () => {
      const b = await s.execute(
        `return [...document.querySelectorAll(".reading button")].map((b) => b.textContent).find((t) => t.includes("Read the"));`,
      );
      return b?.includes("Read the box") && b;
    }, "the Read button to say box");
    assert.match(label, /Read the box/);
    await s.execute(`document.querySelector("canvas").focus();`);
    await s.press("\uE007");
    // A read of another selection starts a fresh list.
    await until(async () => {
      const r = await readings();
      return r.length === 1 && r[0] !== "seen 640x480";
    }, "the box's reading");
    const [w, h] = (await readings())[0].match(/(\d+)x(\d+)/).slice(1).map(Number);
    assert.ok(w > 8 && w < 640 && h > 8 && h < 480, `read a ${w}x${h} box`);
    await s.press(KEY.Escape);
    await until(async () => (await s.findAll("svg.overlay polygon.selection")).length === 0, "the box cleared");
  });
});
