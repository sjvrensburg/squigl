// Screenshots report.html in WebKitGTK's MiniBrowser through WebKitWebDriver (on
// port 4445), in slices, as report-N.png.
import { writeFileSync } from "node:fs";
const base = "http://127.0.0.1:4445";
async function call(method, path, body) {
  const r = await fetch(base + path, { method, headers: { "content-type": "application/json" }, body: body && JSON.stringify(body) });
  const j = await r.json();
  if (!r.ok) throw new Error(JSON.stringify(j));
  return j.value;
}
const s = await call("POST", "/session", {
  capabilities: { alwaysMatch: { "webkitgtk:browserOptions": { binary: "/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1/MiniBrowser", args: ["--automation"] } } },
});
const id = s.sessionId;
try {
  await call("POST", `/session/${id}/window/rect`, { width: 1400, height: 1000 });
  await call("POST", `/session/${id}/url`, { url: `file://${process.cwd()}/report.html` });
  await new Promise((r) => setTimeout(r, 1500));
  const total = await call("POST", `/session/${id}/execute/sync`, { script: "return document.body.scrollHeight", args: [] });
  let n = 0;
  for (let y = 0; y < total; y += 900, n++) {
    await call("POST", `/session/${id}/execute/sync`, { script: `window.scrollTo(0, ${y})`, args: [] });
    await new Promise((r) => setTimeout(r, 300));
    writeFileSync(`report-${n}.png`, Buffer.from(await call("GET", `/session/${id}/screenshot`), "base64"));
  }
  console.log(`${n} slices`);
} finally {
  await call("DELETE", `/session/${id}`);
}
