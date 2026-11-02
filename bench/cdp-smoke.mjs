// Smoke test kontrak CDP: jalankan terhadap endpoint daemon yang sudah hidup.
//
//   node bench/cdp-smoke.mjs [--endpoint http://127.0.0.1:9224] [--url https://example.com]
//
// Yang dibuktikan, dalam urutan yang sama dengan yang dipakai klien nyata:
//   1. /json/version menjawab dan URL websocket-nya menunjuk ke endpoint ini,
//      bukan ke port mesin.
//   2. Websocket browser bisa dibuka.
//   3. Target.createTarget + attachToTarget(flatten) memberi sessionId.
//   4. Page.navigate + Runtime.evaluate mengembalikan judul halaman.
//   5. Target.closeTarget menutup tab.
//
// Keluar 0 hanya kalau semua lolos. Skrip ini tidak menyalakan apa pun:
// daemon yang dites harus sudah berjalan, supaya yang diukur benar-benar
// jalur proxy, bukan jalur mesin.
const argv = process.argv.slice(2);
function arg(name, fallback) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
}

const endpoint = arg('--endpoint', 'http://127.0.0.1:9224').replace(/\/$/, '');
const url = arg('--url', 'https://example.com');
const expectTitle = arg('--expect', 'Example Domain');
const timeoutMs = Number(arg('--timeout-ms', '30000'));

const fail = (step, detail) => {
  console.error(`GAGAL [${step}] ${detail}`);
  process.exit(1);
};
const ok = (step, detail = '') => console.log(`ok    [${step}] ${detail}`);

const withTimeout = (promise, ms, label) =>
  Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timeout ${ms}ms`)), ms)),
  ]);

// 1. Discovery
let version;
try {
  version = await (await fetch(`${endpoint}/json/version`)).json();
} catch (err) {
  fail('json/version', String(err));
}
if (!version.webSocketDebuggerUrl) fail('json/version', 'webSocketDebuggerUrl kosong');
const endpointHost = new URL(endpoint).host;
if (!version.webSocketDebuggerUrl.includes(endpointHost)) {
  fail(
    'json/version',
    `webSocketDebuggerUrl menunjuk ke ${version.webSocketDebuggerUrl}, bukan ${endpointHost}`,
  );
}
ok('json/version', version.Browser);

// 2. Websocket
let ws;
try {
  ws = await withTimeout(
    new Promise((resolve, reject) => {
      const socket = new WebSocket(version.webSocketDebuggerUrl);
      socket.onopen = () => resolve(socket);
      socket.onerror = () => reject(new Error('websocket ditolak'));
    }),
    timeoutMs,
    'websocket',
  );
} catch (err) {
  fail('websocket', String(err));
}
ok('websocket', version.webSocketDebuggerUrl);

let idc = 1;
const pending = new Map();
ws.addEventListener('message', (ev) => {
  let msg;
  try {
    msg = JSON.parse(ev.data);
  } catch {
    return;
  }
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  }
});
const send = (method, params = {}, sessionId) =>
  withTimeout(
    new Promise((resolve) => {
      const id = idc++;
      pending.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    }),
    timeoutMs,
    method,
  );

// 3. Target + session
const created = await send('Target.createTarget', { url: 'about:blank' });
if (!created.result?.targetId) fail('Target.createTarget', JSON.stringify(created.error ?? created));
const targetId = created.result.targetId;
ok('Target.createTarget', targetId);

const attached = await send('Target.attachToTarget', { targetId, flatten: true });
const sessionId = attached.result?.sessionId;
if (!sessionId) fail('Target.attachToTarget', JSON.stringify(attached.error ?? attached));
ok('Target.attachToTarget', sessionId);

// 4. Navigasi + evaluasi
await send('Page.enable', {}, sessionId);
await send('Runtime.enable', {}, sessionId);
const nav = await send('Page.navigate', { url }, sessionId);
if (nav.error) fail('Page.navigate', JSON.stringify(nav.error));
// Tunggu load, bukan tidur tetap: dokumen yang belum jadi membuat evaluasi
// berikutnya mengembalikan judul kosong dan terlihat seperti bug proxy.
for (let i = 0; i < 100; i++) {
  const state = await send('Runtime.evaluate', { expression: 'document.readyState', returnByValue: true }, sessionId);
  if (state.result?.result?.value === 'complete') break;
  await new Promise((r) => setTimeout(r, 100));
}
const evaluated = await send(
  'Runtime.evaluate',
  { expression: 'document.title', returnByValue: true },
  sessionId,
);
const title = evaluated.result?.result?.value;
if (evaluated.error) fail('Runtime.evaluate', JSON.stringify(evaluated.error));
ok('Runtime.evaluate', JSON.stringify(title));
if (expectTitle && title !== expectTitle) {
  fail('judul', `dapat ${JSON.stringify(title)}, harap ${JSON.stringify(expectTitle)}`);
}

// 5. Tutup
await send('Target.closeTarget', { targetId });
ok('Target.closeTarget', targetId);
ws.close();

console.log(`\nSEMUA LOLOS lewat ${endpoint}`);
process.exit(0);