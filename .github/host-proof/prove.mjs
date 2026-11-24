// Fork only, never offered upstream: proves the host extension points in the real wasm build
// before a release is published (host-release.yml). A bare page host, made here from the
// release's own index.html (data-host="page" on the canvas and a log of every wordcraft:* event
// the app sends), drives each slot in headless Chrome as a hosting page would.
//   npm install && node prove.mjs <unzipped wordcraft-web-<version>/> <out dir> [chrome]
import { createServer } from 'node:http';
import { readFile, stat, writeFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import puppeteer from 'puppeteer-core';
import { zipSync, strToU8 } from 'fflate';

const [SITE, OUT, CHROME = '/usr/bin/google-chrome'] = process.argv.slice(2);

// The host page: the release's index.html, hosted in its own document, logging what the app sends.
const LOG = `<script>
window.__log = [];
{
  const c = document.getElementById('wordcraft_canvas');
  for (const k of ['ready', 'save', 'dirty', 'failed', 'open-request']) c.addEventListener('wordcraft:' + k, (e) => {
    const d = e.detail || {};
    window.__log.push({ kind: k, ...Object.fromEntries(Object.entries(d).filter(([n]) => n !== 'bytes')), bytes: d.bytes ? d.bytes.byteLength : undefined, at: performance.now() });
  });
}
</script>
`;
const index = await readFile(join(SITE, 'index.html'), 'utf8');
if (!index.includes('<canvas id="wordcraft_canvas"></canvas>')) throw new Error('index.html has no plain wordcraft_canvas to host');
await writeFile(join(SITE, 'host.html'), index.replace('<canvas id="wordcraft_canvas"></canvas>', '<canvas id="wordcraft_canvas" data-host="page"></canvas>').replace('</body>', LOG + '</body>'));
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css' };
const WORDS = 'Report paragraph from GitHub';
const docx = zipSync({
  '[Content_Types].xml': strToU8('<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'),
  '_rels/.rels': strToU8('<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'),
  'word/document.xml': strToU8(`<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>${WORDS}</w:t></w:r></w:p></w:body></w:document>`),
});

const server = createServer(async (req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const file = normalize(join(SITE, path));
  try { await stat(file); res.writeHead(200, { 'Content-Type': TYPES[extname(file)] ?? 'application/octet-stream' }).end(await readFile(file)); }
  catch { res.writeHead(404).end(); }
});
await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
const browser = await puppeteer.launch({ executablePath: CHROME, headless: true,
  args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--no-sandbox'] });

const results = [];
let failed = false;
const check = (name, ok, evidence) => {
  results.push({ name, ok, evidence });
  if (!ok) failed = true;
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}\n      ${JSON.stringify(evidence)}`);
};

try {
  const page = await browser.newPage();
  await page.setViewport({ width: 1400, height: 900 });
  // Every time the app asks the browser for a file: a file input clicked or put on the page.
  await page.evaluateOnNewDocument(() => {
    window.__filePickers = 0;
    const click = HTMLInputElement.prototype.click;
    HTMLInputElement.prototype.click = function () {
      if (this.type === 'file') window.__filePickers += 1;
      return click.call(this);
    };
    new MutationObserver((records) => {
      for (const r of records) for (const n of r.addedNodes) if (n instanceof Element && (n.matches('input[type=file]') || n.querySelector('input[type=file]'))) window.__filePickers += 1;
    }).observe(document, { childList: true, subtree: true });
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/host.html`);
  await page.waitForFunction(() => window.__log.some((e) => e.kind === 'ready') && typeof window.wordcraft === 'object', { timeout: 180_000 });

  // Helpers in the page: send a host message, run a command, read the log.
  await page.evaluate(() => {
    const c = document.getElementById('wordcraft_canvas');
    window.send = (k, d) => c.dispatchEvent(new CustomEvent('wordcraft:' + k, { detail: d }));
    window.run = (id, p = {}) => window.wordcraft.execute(id, p).then((v) => ({ ok: v }), (e) => ({ err: String(e) }));
    window.since = (t) => window.__log.filter((e) => e.at > t);
  });
  const now = () => page.evaluate(() => performance.now());
  const run = (id, p) => page.evaluate((i, q) => window.run(i, q), id, p);
  const send = (k, d) => page.evaluate((a, b) => window.send(a, b), k, d);
  const since = (t) => page.evaluate((x) => window.since(x), t);
  const pause = (ms) => new Promise((ok) => setTimeout(ok, ms));
  const shows = async (w) => {
    for (let i = 0; i < 60; i++) {
      const t = await page.evaluate(() => Promise.race([window.wordcraft.docInspect({ text: true }).then((d) => JSON.stringify(d), () => ""), new Promise((r) => setTimeout(() => r(''), 2000))]));
      if (t.includes(w)) return;
      await new Promise((ok) => setTimeout(ok, 1000));
    }
    throw new Error('the document never showed ' + w);
  };

  // ── Slot 1: host places ──────────────────────────────────────────────────────────────
  check('no places before the host declares any', (await run('ui.hostPlaces')).ok?.places?.length === 0, await run('ui.hostPlaces'));
  await send('places', { places: [
    { id: 'github', label: 'GitHub', open: true, saveAs: true },
    { id: 'inbox', label: 'Shared inbox', open: true, saveAs: false },
    { id: 'bad', label: 'Neither' },
  ] });
  await pause(300);
  const listed = (await run('ui.hostPlaces')).ok;
  check('wordcraft:places is listed (the entry offering neither is dropped)', listed?.places?.map((p) => p.id).join() === 'github,inbox', listed);

  await run('ui.backstage', { value: false });
  await run('ui.openFileDialog');
  await pause(800);
  const owned = await page.evaluate(() => ({ filePickers: window.__filePickers }));
  check('a host with open places owns Open: no browser picker is asked for', owned.filePickers === 0, owned);
  await page.screenshot({ path: join(OUT, '1-backstage-open-places.png') });
  await run('ui.backstage', { value: false });

  let t = await now();
  const asked = await run('ui.hostOpen', { place: 'github' });
  await pause(200);
  const requests = (await since(t)).filter((e) => e.kind === 'open-request');
  check('choosing a place sends wordcraft:open-request {place}', asked.ok?.asked === 'github' && requests.length === 1 && requests[0].place === 'github', { asked, requests });

  // The host answers with the document and where it lives.
  await page.evaluate((bytes) => window.send('open', { name: 'Report.docx', bytes: new Uint8Array(bytes).buffer }), Array.from(docx));
  await send('document-place', { place: 'github', key: 'acme/papers@main:docs/Report.docx', label: 'acme/papers · main · docs/Report.docx' });
  await shows(WORDS);
  await pause(500);
  const where = (await run('ui.hostPlaces')).ok?.document;
  check('wordcraft:document-place is the open document\'s place', where?.label === 'acme/papers · main · docs/Report.docx' && where?.key === 'acme/papers@main:docs/Report.docx', where);

  await run('ui.backstage', { value: true, page: 'info' });
  await pause(800);
  await page.screenshot({ path: join(OUT, '2-backstage-info-location.png') });
  await run('ui.backstage', { value: true, page: 'open' });
  await pause(800);
  await page.screenshot({ path: join(OUT, '3-backstage-open-recent.png') });

  await run('ui.backstage', { value: false });
  await pause(300);
  // ── Slot 3: save outcome ─────────────────────────────────────────────────────────────
  await run('text.insert', { text: 'Edited. ' });
  t = await now();
  let r = await run('file.save');
  await pause(300);
  let saves = (await since(t)).filter((e) => e.kind === 'save');
  check('before confirm-saves a save carries no id (unchanged behaviour)', r.ok && saves.length === 1 && saves[0].id === undefined, { r, saves });

  await send('confirm-saves', { value: true });
  await run('text.insert', { text: 'Again. ' });
  t = await now();
  r = await run('file.save');
  await pause(300);
  let log = await since(t);
  saves = log.filter((e) => e.kind === 'save');
  const id1 = saves[0]?.id;
  check('after confirm-saves a save carries an id and is pending', typeof id1 === 'string' && r.ok?.pendingSave !== undefined, { r, saves });

  const heardClean = (await since(t)).some((e) => e.kind === 'dirty' && e.dirty === false);
  check('while a save waits for its answer the host never hears the document is clean', !heardClean, await since(t));
  const pendingClose = await run('file.close');
  check('a save the host has not confirmed still counts as unsaved (Close is refused)', pendingClose.err !== undefined, pendingClose);
  t = await now();
  await send('save-failed', { id: id1, message: 'the branch has moved on' });
  await pause(800);
  log = await since(t);
  // The host never heard "clean", so it hears nothing new: the document is simply still unsaved.
  const afterFailure = await run('file.close');
  check('wordcraft:save-failed leaves the document unsaved (the host never hears clean; Close is still refused)', !log.some((e) => e.kind === 'dirty' && e.dirty === false) && afterFailure.err !== undefined, { log, afterFailure });
  await pause(500);
  await page.screenshot({ path: join(OUT, '4-save-failed-message.png') });

  t = await now();
  r = await run('file.save');
  await pause(300);
  const id2 = (await since(t)).find((e) => e.kind === 'save')?.id;
  const before = await run('file.close');
  t = await now();
  await send('saved', { id: id2 });
  await pause(800);
  check('wordcraft:saved {id} tells the host the document is clean', (await since(t)).some((e) => e.kind === 'dirty' && e.dirty === false), await since(t));
  check('wordcraft:saved {id} confirms it: refused before the answer, closes after', typeof id2 === 'string' && id2 !== id1 && before.err !== undefined, { id2, before });

  await run('text.insert', { text: 'To another place. ' });
  t = await now();
  r = await run('ui.hostSaveAs', { place: 'github' });
  await pause(300);
  saves = (await since(t)).filter((e) => e.kind === 'save');
  check('a Save As into a place carries place and saveAs', r.ok && saves.length === 1 && saves[0].place === 'github' && saves[0].saveAs === true, { r, saves });
  if (saves[0]?.id) await send('saved', { id: saves[0].id });
  check('Save As into a place that takes no saves is refused', (await run('ui.hostSaveAs', { place: 'inbox' })).err !== undefined, await run('ui.hostSaveAs', { place: 'inbox' }));
  await pause(500);
  const closed = await run('file.close');
  check('with every save confirmed, the document closes', closed.ok?.closed === true, closed);

  // ── Slot 1 again: the Recent entry reopens through the host ──────────────────────────
  // Clicked on the backstage Open page's Recent row, as a person would.
  await run('ui.backstage', { value: true, page: 'open' });
  await pause(800);
  t = await now();
  // The Recent row is found, not assumed: down the Open page's column, the first click that asks
  // the host for a document by key is the row (the place buttons above it ask without a key; the
  // list's Clear all sits below it). Fonts and a hidden Browse… move it from build to build.
  const row = [318, 0];
  if (row) {
    let reopen = [];
    for (let y = 100; y <= 400 && reopen.length === 0; y += 12) {
      await page.mouse.click(row[0], y);
      await pause(250);
      reopen = (await since(t)).filter((e) => e.kind === 'open-request' && e.key !== undefined);
      await run('ui.backstage', { value: true, page: 'open' });
    }
    check('a Recent document from a place reopens through wordcraft:open-request {place, key}', reopen.length === 1 && reopen[0].key === 'acme/papers@main:docs/Report.docx', reopen);
    await page.evaluate((bytes) => window.send('open', { name: 'Report.docx', bytes: new Uint8Array(bytes).buffer }), Array.from(docx));
    await send('document-place', { place: 'github', key: 'acme/papers@main:docs/Report.docx', label: 'acme/papers · main · docs/Report.docx' });
    await shows(WORDS);
    await page.screenshot({ path: join(OUT, '5-after-recent-click.png') });
  } else console.log('SKIP  Recent click: set RECENT_ROW=[x,y] from 3-backstage-open-recent.png');
  await run('ui.backstage', { value: false });

  // ── Slot 2: AutoSave ─────────────────────────────────────────────────────────────────
  const refused = await run('file.autosave', { value: true });
  check('without wordcraft:autosave, AutoSave stays off with the browser reason', /browser/i.test(refused.err ?? ''), refused);
  await send('autosave', { available: true });
  await pause(300);
  const on = await run('file.autosave', { value: true });
  check('wordcraft:autosave {available: true} lets AutoSave turn on', on.ok?.value === true, on);
  await page.screenshot({ path: join(OUT, '6-autosave-on.png') });
  await run('text.insert', { text: 'Typed with AutoSave on. ' });
  t = await now();
  await pause(6000);
  saves = (await since(t)).filter((e) => e.kind === 'save');
  check('AutoSave writes through the host on its own (no Save pressed)', saves.length >= 1 && saves[0].name === 'Report.docx' && typeof saves[0].id === 'string', saves);
  for (const s of saves) await send('saved', { id: s.id });
  await send('autosave', { available: false });
  await pause(300);
  const off = await run('file.autosave', { value: true });
  check('the host can take AutoSave back', off.err !== undefined, off);
} finally {
  await writeFile(join(OUT, 'results.json'), JSON.stringify(results, null, 1));
  await browser.close();
  server.close();
}
console.log(failed ? 'SOME CHECKS FAILED' : 'ALL CHECKS PASSED');
process.exit(failed ? 1 : 0);
