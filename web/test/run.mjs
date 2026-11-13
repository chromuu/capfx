// Headless test suite for the composite capture shader.
import { chromium } from "/opt/npm-tools/node_modules/playwright/index.mjs";
import fs from "fs";
import path from "path";

const here = path.dirname(new URL(import.meta.url).pathname);
const page_src = fs.readFileSync(path.join(here, "..", "index.html"), "utf8");
const wrapped = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"></head><body>${page_src}</body></html>`;
const htmlPath = path.join(here, "page.html");
fs.writeFileSync(htmlPath, wrapped);
const outDir = path.join(here, "out"); fs.mkdirSync(outDir, { recursive: true });

const browser = await chromium.launch({
  executablePath: "/opt/pw-browsers/chromium-1194/chrome-linux/chrome",
  args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
const consoleErrors = [];
page.on("console", m => { if (m.type() === "error") consoleErrors.push(m.text()); });
page.on("pageerror", e => consoleErrors.push(String(e)));
await page.goto("file://" + htmlPath + "?noauto");
await page.waitForFunction(() => window.CAP, null, { timeout: 30000 });

// in-page helpers
await page.evaluate(() => {
  const W = 640, H = 480;
  window.TT = {
    W, H,
    // metric helpers on stage buffers (GL bottom-origin rows)
    px(buf, x, y) { const i = (y * buf.w + x) * 4; return [buf.data[i], buf.data[i+1], buf.data[i+2]]; },
    // map a source-canvas coordinate (top-origin, 640x480) to capture raster (720x480 bottom-origin)
    toCap(sx, sy) { return [Math.round(8 + sx * 704 / 640), 479 - Math.round(sy)]; },
    luma(c) { return 0.299*c[0] + 0.587*c[1] + 0.114*c[2]; },
    chromaMag(c) { return Math.max(...c) - Math.min(...c); },
    clean(extra = {}) {
      CAP.set(Object.assign({}, CAP.PRESETS["Clean reference"], { mpeg: false, overscan: 0, view: "final", wipe: false }, extra));
    },
  };
  CAP.stop();
});

const results = [];
async function test(name, fn) {
  const t0 = Date.now();
  try {
    const r = await page.evaluate(fn);
    results.push({ name, pass: r.pass, detail: r.detail, ms: Date.now() - t0 });
  } catch (e) {
    results.push({ name, pass: false, detail: "threw: " + e.message.split("\n")[0], ms: Date.now() - t0 });
  }
  const last = results[results.length - 1];
  console.log(`${last.pass ? "PASS" : "FAIL"}  ${name}  (${last.ms} ms)\n      ${last.detail}`);
}

// ------------------------------------------------------------------ 1
await test("Shaders compile, float targets render, no GL errors", () => {
  TT.clean(); CAP.step(2);
  const e = CAP.glError();
  const buf = CAP.read("final");
  let nan = 0; for (const v of buf.data) if (!Number.isFinite(v)) nan++;
  return { pass: e === 0 && nan === 0, detail: `format ${CAP.format}, glError ${e}, non-finite values ${nan}` };
});

// ------------------------------------------------------------------ 2
await test("Clean round trip preserves flat colours (MPEG off)", () => {
  const cols = [[191,191,191],[191,191,0],[0,191,191],[0,191,0],[191,0,191],[191,0,0],[0,0,191],[128,64,32],[20,20,20]];
  CAP.setSource((c) => { cols.forEach((k,i) => { c.fillStyle = `rgb(${k})`; c.fillRect((i%3)*213, Math.floor(i/3)*160, 214, 160); }); });
  TT.clean(); CAP.step(3);
  const buf = CAP.read("final"); let worst = 0, sum = 0;
  cols.forEach((k,i) => {
    const [x,y] = TT.toCap((i%3)*213 + 107, Math.floor(i/3)*160 + 80);
    const p = TT.px(buf, x, y);
    const err = Math.max(...p.map((v,j)=>Math.abs(v*255 - k[j]))); worst = Math.max(worst, err); sum += err;
  });
  return { pass: worst < 6, detail: `worst channel error ${worst.toFixed(2)}/255, mean ${(sum/cols.length).toFixed(2)}/255 across 9 patches` };
});

// ------------------------------------------------------------------ 3
await test("Clean round trip with MPEG-2 at q=2 stays close", () => {
  TT.clean({ mpeg: true, q: 2 }); CAP.step(1);
  const buf = CAP.read("final");
  const cols = [[191,191,191],[191,191,0],[0,191,191],[0,191,0],[191,0,191],[191,0,0],[0,0,191],[128,64,32],[20,20,20]];
  let worst = 0;
  cols.forEach((k,i) => { const [x,y] = TT.toCap((i%3)*213 + 107, Math.floor(i/3)*160 + 80); const p = TT.px(buf,x,y);
    worst = Math.max(worst, ...p.map((v,j)=>Math.abs(v*255 - k[j]))); });
  return { pass: worst < 8, detail: `worst channel error ${worst.toFixed(2)}/255` };
});

// ------------------------------------------------------------------ 4
await test("Subcarrier phase error is cancelled by the matching tint", () => {
  TT.clean({ hue: 20, tint: 0 }); CAP.step(1);
  const [x,y] = TT.toCap(107+426, 240); // magenta patch (191,0,191) at i=4 -> col1,row1 ; use red i=5: col2,row1
  const bad = TT.px(CAP.read("final"), x, y);
  TT.clean({ hue: 20, tint: -20 }); CAP.step(1);
  const fixed = TT.px(CAP.read("final"), x, y);
  const eB = Math.max(...bad.map((v,j)=>Math.abs(v*255-[191,0,0][j])));
  const eF = Math.max(...fixed.map((v,j)=>Math.abs(v*255-[191,0,0][j])));
  return { pass: eB > 25 && eF < 6, detail: `red patch error with 20° phase error ${eB.toFixed(1)}/255, after -20° tint ${eF.toFixed(1)}/255` };
});

// ------------------------------------------------------------------ 5
await test("Dot crawl: notch decoder animates luma at vertical colour edges; 3-line comb cancels it", () => {
  // vertical red|cyan|blue|yellow stripes (constant down the frame)
  CAP.setSource((c) => { const k = ["#c00000","#00c0c0","#0000c0","#c0c000"]; k.forEach((col,i)=>{ c.fillStyle = col; c.fillRect(i*160,0,160,480); }); });
  function crawl(mode) {
    TT.clean({ ycMode: mode, ycMHz: 1.3 }); CAP.goto(10); const a = CAP.read("decoded").data.slice();
    CAP.goto(11); const b = CAP.read("decoded").data;
    let edge = 0, ne = 0, flat = 0, nf = 0;
    for (let y = 100; y < 380; y++) for (let x = 20; x < 700; x++) {
      const i = (y*720+x)*4; const d = Math.abs(TT.luma([a[i],a[i+1],a[i+2]]) - TT.luma([b[i],b[i+1],b[i+2]]));
      const sx = (x - 8) * 640/704; const nearEdge = [160,320,480].some(e => Math.abs(sx-e) < 12);
      if (nearEdge) { edge += d; ne++; } else if ([160,320,480].every(e => Math.abs(sx-e) > 40)) { flat += d; nf++; }
    }
    return { edge: edge/ne*255, flat: flat/nf*255 };
  }
  const n = crawl(0), c3 = crawl(2);
  return { pass: n.edge > 3 && n.edge > 4*c3.edge && n.flat < 1, detail: `frame-to-frame luma change at edges: notch ${n.edge.toFixed(2)}, 3-line comb ${c3.edge.toFixed(3)} (flat areas: notch ${n.flat.toFixed(3)}) /255` };
});

// ------------------------------------------------------------------ 6
await test("Hanging dots: comb filters leak at horizontal colour edges, notch doesn't", () => {
  CAP.setSource((c) => { c.fillStyle = "#c00000"; c.fillRect(0,0,640,240); c.fillStyle = "#00c0c0"; c.fillRect(0,240,640,240); });
  function hd(mode) {
    TT.clean({ ycMode: mode }); CAP.goto(10); const buf = CAP.read("decoded");
    // luma ripple along the rows nearest the edge: std dev of luma across x
    let worst = 0;
    for (let y = 236; y < 245; y++) {
      const ys = 479 - y; let m = 0, m2 = 0, n = 0;
      for (let x = 60; x < 660; x++) { const l = TT.luma(TT.px(buf,x,ys)); m += l; m2 += l*l; n++; }
      worst = Math.max(worst, Math.sqrt(Math.max(0, m2/n - (m/n)**2)) * 255);
    }
    return worst;
  }
  const notch = hd(0), c2 = hd(1), c3 = hd(2);
  return { pass: c2 > 4*notch + 1 && c3 > 4*notch + 1, detail: `peak luma ripple on edge rows: notch ${notch.toFixed(2)}, 2-line ${c2.toFixed(2)}, 3-line ${c3.toFixed(2)} /255` };
});

// ------------------------------------------------------------------ 7
await test("Cross-colour: 3.58 MHz b/w detail turns to rainbows through a notch, not a comb", () => {
  // vertical b/w stripes at ~3.58 MHz in the source (12.27 px per us across 640)
  CAP.setSource((c) => { const img = c.createImageData(640,480); const f = 3.58/(640/52.15);
    for (let x=0;x<640;x++){ const v = 128 + 110*Math.sin(2*Math.PI*f*x); for (let y=0;y<480;y++){ const i=(y*640+x)*4; img.data[i]=img.data[i+1]=img.data[i+2]=v; img.data[i+3]=255; } }
    c.putImageData(img,0,0); });
  function cc(mode) { TT.clean({ ycMode: mode }); CAP.goto(4); const buf = CAP.read("decoded"); let s = 0, n = 0;
    for (let y=100;y<380;y+=3) for (let x=100;x<620;x++){ s += TT.chromaMag(TT.px(buf,x,y)); n++; } return s/n*255; }
  const n = cc(0), c3 = cc(2);
  return { pass: n > 20 && c3 < n/4, detail: `mean false-colour saturation: notch ${n.toFixed(1)}, 3-line comb ${c3.toFixed(1)} /255` };
});

// ------------------------------------------------------------------ 8
await test("Chroma bleed: red/blue edge is softer in chroma than luma and tracks chroma bandwidth", () => {
  CAP.setSource((c) => { c.fillStyle = "#1010e0"; c.fillRect(0,0,640,480); c.fillStyle = "#e01010"; c.fillRect(320,0,320,480); });
  function width(mhz, chan) { TT.clean({ chromaMHz: mhz }); CAP.goto(2); const buf = CAP.read("decoded"); const y = 240;
    const row = []; for (let x=250;x<470;x++) row.push(chan === "R" ? TT.px(buf,x,y)[0] : TT.luma(TT.px(buf,x,y)));
    const lo = Math.min(...row.slice(0,30)), hi = Math.max(...row.slice(-30));
    const a = row.findIndex(v => v > lo + 0.1*(hi-lo)), b = row.findIndex(v => v > lo + 0.9*(hi-lo)); return b - a; }
  const wide = width(2.0, "R"), narrow = width(0.5, "R"), yw = width(1.1, "Y");
  return { pass: narrow > wide && narrow >= 6 && wide > yw, detail: `10–90% rise in samples: R @0.5 MHz ${narrow}, R @2.0 MHz ${wide}, luma ${yw}` };
});

// ------------------------------------------------------------------ 9
await test("MPEG-2 quantisation produces 8×8 blocking that grows with q", () => {
  CAP.setSource((c) => { const g = c.createLinearGradient(0,0,640,480); g.addColorStop(0,"#202060"); g.addColorStop(1,"#e0c080"); c.fillStyle = g; c.fillRect(0,0,640,480);
    for (let i=0;i<220;i++){ c.fillStyle = `hsl(${(i*47)%360},60%,${30+(i*13)%50}%)`; const x=(i*97)%640, y=(i*53)%480; c.beginPath(); c.arc(x,y,6+(i%9)*3,0,7); c.fill(); } });
  function blockiness(q) { TT.clean({ mpeg: true, q }); CAP.goto(15); const b = CAP.read("luma"); let on = 0, non = 0, off = 0, noff = 0;
    for (let y=16;y<464;y++) for (let x=16;x<704;x++){ const d = Math.abs(b.data[(y*720+x)*4] - b.data[(y*720+x-1)*4]);
      if (x % 8 === 0) { on += d; non++; } else { off += d; noff++; } }
    return (on/non)/(off/noff); }
  const q2 = blockiness(2), q40 = blockiness(40);
  return { pass: q40 > 1.4*q2, detail: `boundary/interior gradient ratio: q=2 ${q2.toFixed(2)}, q=40 ${q40.toFixed(2)}` };
});

// ------------------------------------------------------------------ 10
await test("Interlace: weave combs a moving object; bob removes the combing", () => {
  CAP.setSource((c, t) => { c.fillStyle = "#000"; c.fillRect(0,0,640,480); c.fillStyle = "#fff"; c.fillRect(100 + t*1200 % 400, 140, 80, 200); });
  function comb(deint) { TT.clean({ deint }); CAP.goto(7); const b = CAP.read("final"); let s = 0, n = 0;
    for (let y=150;y<320;y++) for (let x=20;x<700;x++){ const l = i => TT.luma(TT.px(b,x,i)); s += Math.abs(2*l(y) - l(y-1) - l(y+1)); n++; } return s/n*255; }
  const weave = comb(0), bob = comb(1), blend = comb(2);
  return { pass: weave > 3*bob && weave > 3*blend, detail: `mean vertical comb energy: weave ${weave.toFixed(2)}, bob ${bob.toFixed(2)}, blend ${blend.toFixed(2)}` };
});

// ------------------------------------------------------------------ 11
await test("Chroma upsampling bug smears colour between fields only on motion", () => {
  const scene = (moving) => (c, t) => { c.fillStyle = "#202020"; c.fillRect(0,0,640,480); c.fillStyle = "#e01010"; c.fillRect(moving ? 120 + t*1500 % 300 : 200, 100, 120, 280); };
  function diff(moving) { CAP.setSource(scene(moving));
    TT.clean({ mpeg: true, q: 2, cue: false }); CAP.goto(9); const a = CAP.read("final").data.slice();
    TT.clean({ mpeg: true, q: 2, cue: true }); CAP.goto(9); const b = CAP.read("final").data;
    let s = 0; for (let i=0;i<a.length;i+=4) s += Math.abs(a[i]-b[i]); return s/(a.length/4)*255; }
  const st = diff(false), mv = diff(true);
  return { pass: mv > 4*st && mv > 0.3, detail: `mean red-channel change from CUE: static ${st.toFixed(3)}, moving ${mv.toFixed(3)} /255` };
});

// ------------------------------------------------------------------ 12
await test("Black level: setup mismatch raises or crushes blacks", () => {
  CAP.setSource((c) => { c.fillStyle = "#000"; c.fillRect(0,0,320,480); c.fillStyle = "#1a1a1a"; c.fillRect(320,0,320,480); });
  function lv(levels) { TT.clean({ levels }); CAP.goto(2); const b = CAP.read("final"); const [x0,y] = TT.toCap(160,240), [x1] = TT.toCap(480,240);
    return [TT.px(b,x0,y)[1]*255, TT.px(b,x1,y)[1]*255]; }
  const m = lv(0), r = lv(1), c = lv(2);
  return { pass: Math.abs(m[0]) < 3 && r[0] > 12 && c[1] < m[1] - 10, detail: `black / 10% grey → matched ${m.map(v=>v.toFixed(1))}, raised ${r.map(v=>v.toFixed(1))}, crushed ${c.map(v=>v.toFixed(1))}` };
});

// ------------------------------------------------------------------ 13
await test("Noise is deterministic per frame and scales with the RF noise control", () => {
  CAP.setSource((c) => { c.fillStyle = "#808080"; c.fillRect(0,0,640,480); });
  function sd(noise) { TT.clean({ noise }); CAP.goto(5); const b = CAP.read("composite"); let m=0,m2=0,n=0;
    for (let y=50;y<430;y+=2) for (let x=40;x<720;x+=4){ const v=b.data[(y*764+x)*4]; m+=v; m2+=v*v; n++; } return Math.sqrt(Math.max(0, m2/n-(m/n)**2)); }
  const a = sd(0.04), a2 = sd(0.04), z = sd(0);
  return { pass: Math.abs(a - 0.04) < 0.006 && a === a2 && z < 0.002, detail: `composite σ: set 0.040 → measured ${a.toFixed(4)} (repeat ${a2.toFixed(4)}), noise off ${z.toFixed(4)}` };
});

// ------------------------------------------------------------------ 14
await test("Subcarrier phase flips 180° every frame (dot crawl cadence)", () => {
  CAP.setSource((c) => { c.fillStyle = "#c00000"; c.fillRect(0,0,640,480); });
  TT.clean(); CAP.goto(20); const a = CAP.read("composite").data.slice(); CAP.goto(21); const b = CAP.read("composite").data; CAP.goto(22); const c2 = CAP.read("composite").data;
  let anti = 0, same = 0, n = 0, mean = 0;
  for (let y=100;y<380;y++) for (let x=100;x<660;x++){ const i=(y*764+x)*4; mean += a[i]; n++; }
  mean /= n;
  for (let y=100;y<380;y++) for (let x=100;x<660;x++){ const i=(y*764+x)*4; anti += Math.abs((a[i]-mean)+(b[i]-mean)); same += Math.abs(a[i]-c2[i]); }
  return { pass: anti/n < 1e-3 && same/n < 1e-5, detail: `|f0+f1| chroma residue ${(anti/n).toExponential(2)}, |f0−f2| ${(same/n).toExponential(2)}` };
});

// ------------------------------------------------------------------ perf + screenshots
const perf = await page.evaluate(() => { CAP.setSource(null); CAP.set(Object.assign({}, CAP.PRESETS["Bt878 PCI tuner"], { source: "game", view: "final" }));
  CAP.step(1); const t0 = performance.now(); CAP.step(10); return (performance.now() - t0) / 10; });
console.log(`\nframe time (SwiftShader CPU rasteriser): ${perf.toFixed(0)} ms/frame`);

const shots = [];
async function shot(name, setup) {
  await page.evaluate(setup);
  // headless SwiftShader doesn't composite WebGL into screenshots, so read the canvas itself
  const url = await page.evaluate(() => document.getElementById("gl").toDataURL("image/png"));
  const file = path.join(outDir, name + ".png");
  fs.writeFileSync(file, Buffer.from(url.split(",")[1], "base64")); shots.push(file);
}
const base = (preset, source, view, extra = {}) => `CAP.setSource(null); CAP.set(Object.assign({}, CAP.PRESETS[${JSON.stringify(preset)}], { source: ${JSON.stringify(source)}, view: ${JSON.stringify(view)}, overscan: 0.03, wipe:false }, ${JSON.stringify(extra)})); CAP.goto(47);`;
await shot("game_source", base("Bt878 PCI tuner", "game", "source"));
await shot("game_bt878", base("Bt878 PCI tuner", "game", "final"));
await shot("game_pvr", base("Hardware MPEG-2", "game", "final"));
await shot("game_usb", base("USB 2.0 dongle", "game", "final"));
await shot("bars_bt878", base("Bt878 PCI tuner", "bars", "final"));
await shot("res_bt878", base("Bt878 PCI tuner", "res", "final"));
await shot("res_comb3", base("Hardware MPEG-2", "res", "final"));
await shot("bars_composite", base("Bt878 PCI tuner", "bars", "composite"));
await shot("game_chroma", base("Bt878 PCI tuner", "game", "chroma"));
await shot("game_wipe", base("Bt878 PCI tuner", "game", "final", { wipe: true, split: 0.5 }));
await page.screenshot({ path: path.join(outDir, "page_desktop.png"), fullPage: true });
await page.setViewportSize({ width: 400, height: 900 });
await page.screenshot({ path: path.join(outDir, "page_phone.png"), fullPage: true });

const passed = results.filter(r => r.pass).length;
console.log(`\n${passed}/${results.length} tests passed. Console errors: ${consoleErrors.length ? consoleErrors.join(" | ") : "none"}`);
fs.writeFileSync(path.join(outDir, "results.json"), JSON.stringify({ results, perf, consoleErrors }, null, 2));
await browser.close();
