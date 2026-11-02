// Harness bench: nyalakan satu engine, buka beberapa tab, ukur RSS/PSS per
// tahap, tutup, lalu ukur lagi. Diturunkan dari `/tmp/bx/bench-chrome.mjs`
// yang sudah terbukti mengukur dengan benar.
//
// Usage:
//   node bench/bench-tabs.mjs [--bin PATH] [--port N] [--udd DIR] [--out FILE]
//                             [--urls a,b,c] [--label NAME] [--keep]
//
// Semua angka masuk JSON, bukan cuma ke layar: keputusan anggaran memori
// diambil dari file hasil, bukan dari ingatan orang yang menjalankan.
import { spawn, execSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const DEFAULT_BIN =
  process.env.OCB_ENGINE_BINARY ||
  (() => {
    const base = path.join(process.env.HOME || '.', '.cache/ms-playwright');
    try {
      const dir = fs
        .readdirSync(base)
        .filter((d) => d.startsWith('chromium_headless_shell-'))
        .sort()
        .pop();
      return dir ? path.join(base, dir, 'chrome-headless-shell-linux64/chrome-headless-shell') : null;
    } catch {
      return null;
    }
  })();

const opts = {
  bin: DEFAULT_BIN,
  port: 9333,
  udd: null,
  out: null,
  urls: ['https://example.com', 'https://example.org', 'https://www.iana.org'],
  label: 'shell',
  keep: false,
  idleSleepMs: 2500,
  navSleepMs: 8000,
  closeSleepMs: 5000,
};

const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  const next = () => argv[++i];
  if (a === '--bin') opts.bin = next();
  else if (a === '--port') opts.port = Number(next());
  else if (a === '--udd') opts.udd = next();
  else if (a === '--out') opts.out = next();
  else if (a === '--label') opts.label = next();
  else if (a === '--urls') opts.urls = next().split(',').map((u) => u.trim()).filter(Boolean);
  else if (a === '--keep') opts.keep = true;
  else if (a === '--nav-sleep') opts.navSleepMs = Number(next());
  else if (a === '--help' || a === '-h') {
    console.log(fs.readFileSync(new URL(import.meta.url).pathname, 'utf8').split('\n').slice(4, 9).join('\n'));
    process.exit(0);
  } else {
    console.error(`argumen tidak dikenal: ${a}`);
    process.exit(2);
  }
}

if (!opts.bin || !fs.existsSync(opts.bin)) {
  console.error(`biner engine tidak ditemukan: ${opts.bin ?? '(null)'}`);
  process.exit(2);
}

// Satu sampah lama yang dibiarkan = angka bench berikutnya lebih gelap dari
// kenyataan, karena procs() menghitung anak proses, bukan nama.
const udd = opts.udd || fs.mkdtempSync(path.join(process.env.TMPDIR || '/tmp', 'ocb-bench-'));

const START = Date.now();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function snapshot() {
  const kids = new Map();
  const rss = new Map();
  const pss = new Map();
  const cmd = new Map();
  for (const d of fs.readdirSync('/proc')) {
    if (!/^\d+$/.test(d)) continue;
    const pid = Number(d);
    try {
      const st = fs.readFileSync(`/proc/${d}/stat`, 'utf8');
      const ppid = Number(st.slice(st.lastIndexOf(') ') + 2).split(' ')[1]);
      if (!kids.has(ppid)) kids.set(ppid, []);
      kids.get(ppid).push(pid);
      cmd.set(pid, fs.readFileSync(`/proc/${d}/cmdline`, 'utf8').replace(/\0/g, ' ').trim());
      for (const line of fs.readFileSync(`/proc/${d}/status`, 'utf8').split('\n'))
        if (line.startsWith('VmRSS:')) rss.set(pid, Number(line.split(/\s+/)[1]));
      try {
        for (const line of fs.readFileSync(`/proc/${d}/smaps_rollup`, 'utf8').split('\n'))
          if (line.startsWith('Pss:')) pss.set(pid, Number(line.split(/\s+/)[1]));
      } catch {}
    } catch {}
  }
  return { kids, rss, pss, cmd };
}

// PSS dijumlahkan, bukan dirata-rata: halaman yang dipakai bersama dua proses
// dihitung seperlunya di tiap proses, jadi totalnya tidak dobel. RSS total
// melebih-lebihkan pemakaian nyata, dan itu sudah pernah menipu kita.
function measure(root) {
  const { kids, rss, pss, cmd } = snapshot();
  const seen = [];
  const stack = [root];
  while (stack.length) {
    const p = stack.pop();
    if (seen.includes(p)) continue;
    seen.push(p);
    for (const k of kids.get(p) || []) stack.push(k);
  }
  const sum = (m) => seen.reduce((acc, p) => acc + (m.get(p) || 0), 0) / 1024;
  return {
    procs: seen.length,
    rss_mb: Number(sum(rss).toFixed(1)),
    pss_mb: Number(sum(pss).toFixed(1)),
    biggest: seen
      .map((p) => ({
        pid: p,
        mb: Number(((rss.get(p) || 0) / 1024).toFixed(1)),
        role: (cmd.get(p) || '').match(/--type=([a-z-]+)/)?.[1] || 'browser',
      }))
      .sort((a, b) => b.mb - a.mb)
      .slice(0, 6),
  };
}

function gitSha() {
  try {
    return execSync('git rev-parse --short HEAD', { cwd: REPO, encoding: 'utf8' }).trim();
  } catch {
    return 'nogit';
  }
}

// Flag engine sama dengan daemon supaya angka bench dan angka produksi
// berasal dari konfigurasi yang sama.
const child = spawn(
  opts.bin,
  [
    `--remote-debugging-port=${opts.port}`,
    '--remote-debugging-address=127.0.0.1',
    `--user-data-dir=${udd}`,
    '--no-sandbox',
    '--disable-dev-shm-usage',
    '--disable-gpu',
    '--mute-audio',
    '--window-size=1920,1080',
    ...(process.env.OCB_ENGINE_ARGS ? process.env.OCB_ENGINE_ARGS.split(' ').filter(Boolean) : []),
  ],
  { stdio: ['ignore', 'pipe', 'pipe'], detached: true },
);

let log = '';
child.stdout.on('data', (d) => (log += d));
child.stderr.on('data', (d) => (log += d));
const ROOT = child.pid;

let version = null;
for (let i = 0; i < 80; i++) {
  try {
    version = await (await fetch(`http://127.0.0.1:${opts.port}/json/version`)).json();
    break;
  } catch {
    await sleep(250);
  }
}
if (!version) {
  console.error('engine gagal siap:\n' + log.slice(-2000));
  try {
    process.kill(-ROOT, 'SIGKILL');
  } catch {}
  process.exit(1);
}

const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = reject;
});

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
  new Promise((resolve) => {
    const id = idc++;
    const timer = setTimeout(() => {
      pending.delete(id);
      resolve({ error: { message: 'TIMEOUT' } });
    }, 60_000);
    pending.set(id, (msg) => {
      clearTimeout(timer);
      resolve(msg);
    });
    ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });

const result = {
  engine: version.Browser,
  binary: opts.bin,
  label: opts.label,
  sha: gitSha(),
  started_at: new Date(START).toISOString(),
  user_data_dir: udd,
  stages: {},
  pages: [],
};

await sleep(opts.idleSleepMs);
result.stages.idle = measure(ROOT);

const tabs = [];
for (const url of opts.urls) {
  const t0 = Date.now();
  const created = await send('Target.createTarget', { url: 'about:blank' });
  if (!created.result) {
    result.pages.push({ url, error: JSON.stringify(created.error) });
    continue;
  }
  const attached = await send('Target.attachToTarget', { targetId: created.result.targetId, flatten: true });
  const sid = attached.result?.sessionId;
  if (!sid) {
    result.pages.push({ url, error: `attach gagal: ${JSON.stringify(attached.error)}` });
    continue;
  }
  try {
    await send('Page.enable', {}, sid);
    await send('Runtime.enable', {}, sid);
    const nav = await send('Page.navigate', { url }, sid);
    await sleep(opts.navSleepMs);
    const probe = await send(
      'Runtime.evaluate',
      {
        expression:
          '({t:document.title,text:document.body?document.body.innerText.length:0,heap:performance.memory?performance.memory.usedJSHeapSize:0})',
        returnByValue: true,
      },
      sid,
    );
    tabs.push({ targetId: created.result.targetId, url });
    result.pages.push({
      url,
      ms: Date.now() - t0,
      navErr: nav.error?.message || null,
      probe: probe.result?.result?.value ?? probe.error?.message ?? null,
    });
  } catch (err) {
    result.pages.push({ url, error: String(err).slice(0, 200) });
  }
  await sleep(2000);
  result.stages[`tabs_${tabs.length}`] = measure(ROOT);
}

for (const t of tabs) await send('Target.closeTarget', { targetId: t.targetId });
await sleep(opts.closeSleepMs);
result.stages.after_close = measure(ROOT);

result.finished_at = new Date().toISOString();
result.duration_ms = Date.now() - START;
ws.close();

if (!opts.keep) {
  try {
    process.kill(-ROOT, 'SIGTERM');
  } catch {
    try {
      child.kill('SIGTERM');
    } catch {}
  }
  // Beri waktu proses group benar-benar hilang sebelum keluar, supaya
  // pemanggilan berikutnya tidak mengukur sisa proses ini.
  await sleep(1500);
  try {
    process.kill(-ROOT, 'SIGKILL');
  } catch {}
}

const outPath =
  opts.out ||
  path.join(
    REPO,
    'bench/results',
    `${new Date(START).toISOString().slice(0, 10).replace(/-/g, '')}-${result.sha}-${opts.label}.json`,
  );
fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, JSON.stringify(result, null, 2));

console.log(JSON.stringify(result.stages, null, 2));
for (const p of result.pages) console.log(p.url, p.ms ? `${p.ms}ms` : '', JSON.stringify(p.probe ?? p.error ?? p.navErr));
console.log(`hasil: ${outPath}`);
process.exit(0);