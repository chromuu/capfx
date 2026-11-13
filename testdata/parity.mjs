import { chromium } from "/opt/npm-tools/node_modules/playwright/index.mjs";
import fs from "fs";
const img = "data:image/png;base64," + fs.readFileSync("test_card.png").toString("base64");
const cfgs = { clean: { preset: "Clean reference", extra: { mpeg: false } }, cleanmpeg: { preset: "Clean reference", extra: {} },
  bt878: { preset: "Bt878 PCI tuner", extra: { noise: 0, jitter: 0 } }, usb: { preset: "USB 2.0 dongle", extra: { noise: 0, jitter: 0 } },
  pvr_bob: { preset: "Hardware MPEG-2", extra: { noise: 0, jitter: 0, deint: 1, cue: true } } };
const b = await chromium.launch({ executablePath: "/opt/pw-browsers/chromium-1194/chrome-linux/chrome", args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] });
const p = await b.newPage(); await p.goto("file:///home/claude/capture/test/page.html?noauto"); await p.waitForFunction(() => window.CAP);
await p.evaluate(async (img) => { CAP.stop(); const im = new Image(); im.src = img; await im.decode(); window.__im = im; CAP.setSource((c) => c.drawImage(im, 0, 0)); }, img);
let allOk = true;
for (const [name, cfg] of Object.entries(cfgs)) {
  const got = await p.evaluate(({ cfg }) => { CAP.set(Object.assign({}, CAP.PRESETS[cfg.preset], { overscan: 0 }, cfg.extra)); CAP.goto(47);
    const f = CAP.read("final"), d = CAP.read("decoded"); return { final: Array.from(f.data), decoded: Array.from(d.data), w: f.w, h: f.h }; }, { cfg });
  for (const stage of ["final", "decoded"]) {
    const rust = new Float32Array(fs.readFileSync(`par_${name}_${stage}.f32`).buffer);
    const web = got[stage]; const { w, h } = got;
    let maxd = 0, sum = 0, n = 0, over = 0, maxAt = "";
    for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) for (let c = 0; c < 3; c++) {
      const wv = web[((h - 1 - y) * w + x) * 4 + c], rv = rust[(y * w + x) * 3 + c]; // rust dump is top-row-first
      const d = Math.abs(wv - rv) * 255; sum += d; n++; if (d > maxd) { maxd = d; maxAt = `${x},${y}`; } if (d > 1.0) over++;
    }
    const ok = maxd < 2.0 && over / n < 0.001; allOk &&= ok;
    console.log(`${ok ? "PASS" : "FAIL"} ${name.padEnd(9)} ${stage.padEnd(8)} mean |Δ| ${(sum / n).toFixed(4)}/255  max ${maxd.toFixed(3)}/255 at ${maxAt}  >1/255: ${(100 * over / n).toFixed(4)}%`);
  }
}
console.log(allOk ? "ALL PARITY CHECKS PASSED" : "PARITY FAILURES");
await b.close();
