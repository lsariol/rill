// Drives compare.ps1's Chrome over its debugging port: places the pop-out windows, switches
// YouTube to Live chat, counts the messages each page shows, and reads Chrome's own process list.
// node chrome.mjs <port> setup <left> <top> <width> <height> | reset | read | procs
// Needs Node.js 22 or later (built-in WebSocket). YouTube's chat menu is matched in English.
const [port, cmd, ...rest] = process.argv.slice(2);
const base = `http://127.0.0.1:${port}`;

async function connect(wsUrl) {
  const ws = new WebSocket(wsUrl);
  await new Promise((ok, fail) => { ws.onopen = ok; ws.onerror = fail; });
  let id = 0;
  const waiting = new Map();
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
  };
  const send = (method, params = {}) => new Promise((ok) => {
    const i = ++id; waiting.set(i, ok); ws.send(JSON.stringify({ id: i, method, params }));
  });
  return { send, close: () => ws.close() };
}

async function browserConn() {
  const version = await (await fetch(`${base}/json/version`)).json();
  return connect(version.webSocketDebuggerUrl);
}

async function evaluate(page, expression) {
  const r = await page.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails).slice(0, 300));
  return r.result?.result?.value;
}

function platformOf(url) {
  if (url.includes('twitch.tv')) return 'twitch';
  if (url.includes('youtube.com')) return 'youtube';
  if (url.includes('kick.com')) return 'kick';
  return null;
}

async function pages() {
  const list = await (await fetch(`${base}/json/list`)).json();
  const out = {};
  for (const t of list) {
    const p = t.type === 'page' && platformOf(t.url);
    if (p && !out[p]) out[p] = t;
  }
  return out;
}

// Chrome's own view of its processes: type, pid and CPU seconds.
async function processInfo(browser) {
  const r = await browser.send('SystemInfo.getProcessInfo');
  return r.result.processInfo;
}

// Counts messages added to each chat's own list, without watching the rest of the page.
const COUNTER = `(() => {
  if (window.__rb) return 'exists';
  const rb = window.__rb = { n: 0, since: Date.now() };
  const host = location.host;
  if (host.includes('kick.com')) {
    // Kick keeps a fixed number of rows and renumbers them, so count rows not seen before.
    const row = document.querySelector('[data-index]');
    if (!row) return 'no list';
    const seen = new Set(), order = [];
    const remember = (key) => { seen.add(key); order.push(key); if (order.length > 3000) seen.delete(order.shift()); };
    document.querySelectorAll('[data-index]').forEach(e => remember(e.textContent));
    new MutationObserver(ms => {
      for (const m of ms) for (const x of m.addedNodes) {
        if (x.nodeType !== 1 || !x.hasAttribute('data-index')) continue;
        const key = x.textContent;
        if (!seen.has(key)) { remember(key); rb.n++; }
      }
    }).observe(row.parentElement, { childList: true });
    return 'kick';
  }
  const sel = host.includes('twitch.tv')
    ? '.chat-scrollable-area__message-container'
    : '#items.yt-live-chat-item-list-renderer';
  const box = document.querySelector(sel);
  if (!box) return 'no list';
  new MutationObserver(ms => { for (const m of ms) for (const x of m.addedNodes) if (x.nodeType === 1) rb.n++; })
    .observe(box, { childList: true });
  return 'list';
})()`;

const LIVE_CHAT = `(async () => {
  const label = () => document.querySelector('#view-selector')?.innerText.split('\\n')[0];
  if (label() === 'Live chat') return 'Live chat';
  document.querySelector('#view-selector tp-yt-paper-menu-button, #view-selector #trigger')?.click();
  await new Promise(r => setTimeout(r, 1000));
  [...document.querySelectorAll('tp-yt-paper-listbox a')].find(a => /Live chat/.test(a.innerText))?.click();
  await new Promise(r => setTimeout(r, 1500));
  return label();
})()`;

const SPIN = 'new Promise(r => { const end = performance.now() + 1500; while (performance.now() < end) {} r(1); })';

async function setup([left, top, width, height]) {
  const found = await pages();
  const browser = await browserConn();
  const order = ['twitch', 'youtube', 'kick'];
  const result = { windows: {}, counters: {}, renderer: {} };
  for (const [i, p] of order.entries()) {
    const t = found[p];
    if (!t) { result.windows[p] = 'missing'; continue; }
    const page = await connect(t.webSocketDebuggerUrl);
    if (p === 'youtube') result.youtubeMode = await evaluate(page, LIVE_CHAT);
    result.counters[p] = await evaluate(page, COUNTER);
    const { result: w } = await browser.send('Browser.getWindowForTarget', { targetId: t.id });
    await browser.send('Browser.setWindowBounds', { windowId: w.windowId, bounds: { windowState: 'normal' } });
    await browser.send('Browser.setWindowBounds', {
      windowId: w.windowId,
      bounds: { left: +left + (i + 1) * +width, top: +top, width: +width, height: +height },
    });
    result.windows[p] = w.windowId;
    // Find this page's renderer: keep it busy for 1.5 s and see whose CPU time jumps.
    const before = Object.fromEntries((await processInfo(browser)).map(x => [x.id, x.cpuTime]));
    await evaluate(page, SPIN);
    const after = await processInfo(browser);
    let best = null, gain = 0;
    for (const x of after) {
      const d = x.cpuTime - (before[x.id] ?? x.cpuTime);
      if (d > gain) { gain = d; best = x.id; }
    }
    result.renderer[p] = { pid: best, busyMs: Math.round(gain * 1000) };
    page.close();
  }
  result.processes = await processInfo(browser);
  browser.close();
  console.log(JSON.stringify(result));
}

async function forEachPage(expression) {
  const found = await pages();
  const out = {};
  for (const [p, t] of Object.entries(found)) {
    const page = await connect(t.webSocketDebuggerUrl);
    out[p] = await evaluate(page, expression);
    page.close();
  }
  console.log(JSON.stringify(out));
}

async function procs() {
  const browser = await browserConn();
  console.log(JSON.stringify({ at: Date.now(), processes: await processInfo(browser) }));
  browser.close();
}

if (cmd === 'setup') await setup(rest);
else if (cmd === 'procs') await procs();
else if (cmd === 'reset') await forEachPage('(() => { if (!window.__rb) return null; window.__rb.n = 0; window.__rb.since = Date.now(); return 0; })()');
else if (cmd === 'read') await forEachPage(`(() => ({
  messages: window.__rb ? window.__rb.n : null,
  seconds: window.__rb ? (Date.now() - window.__rb.since) / 1000 : null,
  jsHeapMB: performance.memory ? +(performance.memory.usedJSHeapSize / 1048576).toFixed(1) : null,
  domNodes: document.getElementsByTagName('*').length,
  mode: document.querySelector('#view-selector')?.innerText.split('\\n')[0] ?? null,
}))()`);
else { console.error('Usage: node chrome.mjs <port> setup <left> <top> <width> <height> | reset | read | procs'); process.exit(1); }
process.exit(0);
