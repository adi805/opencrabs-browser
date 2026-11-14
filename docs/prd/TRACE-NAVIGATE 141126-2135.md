# TRACE: `Page.navigate` dari socket sampai piksel

**Tanggal:** 14 Nov 2026 21:35 WIB
**Repo:** `opencrabs-browser` @ `ba65cc7` (`docs(prd): close G10 and record the gate receipt`, 21:18)
**Binari yang ditelusuri:** `/mnt/hdd/OpenCrabs/build/cargo/debug/browserd`, 274.147.368 byte, dibangun 14 Nov 20:59, `sha256:fe287157bf45d8c9…`
**Cara menelusuri:** baca kode (file:line diverifikasi di turn ini) **plus** satu navigasi hidup lewat CDP. Bukan pembacaan statis saja.

---

## 0. Kenapa dokumen ini ada

`Page.navigate` adalah satu-satunya jalur yang menyentuh jaringan dari sisi dokumen. Kalau ada tempat di mana kebijakan jaringan bisa dilewati, di sinilah. Dokumen ini menulis rantainya simpul per simpul supaya klaim keamanan bisa diuji terhadap baris kode, bukan terhadap komentar desain.

**Hasil ringkas:** rantainya utuh dan bekerja (navigasi hidup, semua langkah lolos). **KOREKSI 21:40:** angka "13,8 detik" di §3 BUKAN latensi, lihat catatan koreksi di sana. Tapi ada satu simpul yang **tidak diperiksa kebijakan apa pun**, dan itu bukan bug teoretis: sudah dibuktikan dengan eksperimen diferensial di §5.

---

## 1. Rantai lengkap (18 simpul, semua baris diverifikasi)

| # | Simpul | Lokasi | Yang terjadi |
|---|---|---|---|
| 1 | Terima websocket browser | `crates/cdp/src/native_wire/mod.rs:458` `ws_browser` | `GET /devtools/browser/:id` di-upgrade jadi websocket |
| 2 | Terima websocket halaman | `mod.rs:462` `ws_halaman` | `/devtools/page/:id` → `sesi_halaman(&id)` dicari dulu, sesi tak dikenal dijawab `jawab_galat` (`mod.rs:630`, pemanggil `mod.rs:467`) |
| 3 | Loop satu-frame-satu-jawaban | `mod.rs:474` `layani` | `rx.next()` → teks JSON, `Close`/`Err` memutus loop |
| 4 | Ambil jaringan **sebelum** kunci state | `mod.rs:619` `ambil_untuk_frame` → `ambil_untuk_url` | Hanya `Page.navigate` dan `Target.createTarget` yang memicu jaringan; method lain balik `None` dan nol lalu lintas |
| 5 | Ambil dokumen | `mod.rs:528` `ambil_dokumen` (panggilan jaringan di `:529`) | `jaringan::muat(url, Batas::default())` lalu `gaya::ambil`, `unduh_font`, `ambil_bingkai` |
| 6 | HTTP sungguhan | `crates/cdp/src/native_wire/jaringan.rs:180` `muat` → `:194` `muat_blokir` → `:330` `ambil_badan` | `spawn_blocking`, `ureq` AgentBuilder: connect 5 dtk, total 20 dtk, 5 pengalihan, badan 8 MiB, CSS 1 MiB/16 lembar/2 MiB total, font 8 MiB/8 berkas/24 MiB total |
| 7 | Dispatch CDP | `crates/cdp/src/native_wire/state.rs:509` `"Page.navigate" => self.navigasi(...)` | Method tak dikenal → `-32601`, param salah → `-32602` |
| 8 | Handler navigasi | `state.rs:697` `navigasi` | `tab_dari_sesi` → `param_teks(url)` → `dokumen_awal` |
| 9 | Dokumen awal | `state.rs:1643` `dokumen_awal` | Memilih HTML dari jaringan vs `about:blank`, menyimpan `dok.jaringan` (status, charset, byte) |
| 10 | Ganti dokumen di pool | `crates/engine/src/native/tab_pool.rs:615` `ganti_dokumen_dengan` | Tab dingin dihidupkan dulu, lalu `self.bangun` |
| 11 | Bangun tab panas | `tab_pool.rs:451` `bangun_dengan` | **Layout tidak dihitung di sini** (komentar `:460`), hanya dicatat |
| 12 | Parse HTML | `tab_pool.rs:464` `Document::parse_html` (`dom.rs:121`) | Pohon DOM jadi `Arc<Mutex<…>>`, dipakai bersama isolat dan tab |
| 13 | Halaman + font | `tab_pool.rs:465` `Halaman::baru` (`js.rs:62`) | Viewport + kumpulan font |
| 14 | Isolat | `tab_pool.rs:473` `Isolat::dengan_dokumen_ambil_dan_halaman` | Jembatan DOM + pengambil jaringan skrip (= satu-satunya pemakai `KebijakanJaringan`) |
| 15 | Judul lalu skrip halaman | `tab_pool.rs:476` `dokumen.judul()` → `:482` `pasang_judul` → `:486` `jalankan_skrip_halaman` (`js.rs:1959`, kumpulan skrip `js.rs:162`) | Urutan ini disengaja: skrip yang membaca `document.title` di baris pertama harus melihat judul, bukan string kosong |
| 16 | Kuras timer | `tab_pool.rs:490` `kuras_timer` (`js.rs:2041`) | `setTimeout(fn, 0)` yang dipasang saat muat ikut jalan |
| 17 | Peristiwa + receipt | `state.rs:718` `pasang_bingkai_dari`, `:737` `Network.requestWillBeSent` / `responseReceived`, `:750` `Page.loadEventFired`, `:762` `laporan_skrip`, `:763` `sidik`, `:765` jawaban | Peristiwa dikirim **sebelum** balasan (`mod.rs:491`), alasan tertulis di komentar: klien yang menunggu balasan `evaluate` tidak boleh kehilangan `console.log` |
| 18 | Raster (malas, hanya saat screenshot) | `state.rs:905` `tangkap_layar` → `crates/engine/src/native/paint.rs:224` `lukis_dengan_font` → `:233` `lukis_penuh` → `layout.rs:227` `hitung_dengan_font`; encode `paint.rs:450` `kodekan_png` | Navigasi **tidak** memicu layout. Halaman yang tidak pernah di-screenshot tidak pernah di-layout |

Rantai ini 18 simpul, jauh di atas syarat minimal 8, dan tiap baris dicek ulang di turn yang sama dengan `grep -n`/`sed -n` (dua nomor di ringkasan lama ternyata salah: `ambil_untuk_frame` ada di `:619` bukan `:483`, `kodekan_png` di `:450` bukan `:400`).

---

## 2. Titik gagal per tahap

| Tahap | Kondisi | Gejala ke klien |
|---|---|---|
| 4 (skema) | URL bukan `http`/`https` | `-32000`, tanpa menyentuh jaringan |
| 6 (ambil) | connect > 5 dtk | `Galat` dari `jaringan::muat` → `dokumen_awal` gagal |
| 6 | total > 20 dtk | idem |
| 6 | pengalihan > 5 | idem |
| 6 | badan > 8 MiB | idem (`BATAS_BYTE`) |
| 6 | `content-type` bukan HTML | `-32000`, tidak ada DOM untuk diurai |
| 6 | gaya/font gagal | **tidak fatal**: `LembarJaringan::default()`, halaman tetap tampil tanpa CSS |
| 6 | iframe gagal | **tidak fatal**: masuk `catatan_bingkai`, dilaporkan di `bingkaiCatatan` |
| 8 | sesi tak dikenal | `tab_hilang(tab)` |
| 10 | pool error | `pool_galat` |
| 7 | method tak dikenal / param salah | `-32601` / `-32602` |
| 3 | batas tab | `KODE_BATAS_TAB = -32010` (`crates/cdp/src/batas.rs:49`) |
| 15 | skrip halaman melempar | **tidak fatal**: masuk `skrip.galat`, navigasi tetap sukses |

Pola desainnya jelas dan bagus: **kegagalan yang tidak mematikan tampilan diturunkan jadi catatan, bukan error**. Yang perlu dicatat: `catatan` dan `galat` hanya bisa dibaca kalau klien memang membaca field itu. Klien yang cuma melihat `result` akan mengira semuanya baik-baik saja.

---

## 3. Bukti hidup: satu navigasi penuh

```
$ node bench/cdp-smoke.mjs --endpoint http://127.0.0.1:9250 \
      --url https://example.com --screenshot /tmp/ocb-audit/shot-example.png
ok    [json/version] RustNativeEngine/0.2.0
ok    [websocket] ws://127.0.0.1:9250/devtools/browser/14720618e7efd4a0…
ok    [Target.createTarget] 1
ok    [Target.attachToTarget] 00000000000000000000000000000001
ok    [Runtime.evaluate] "Example Domain"
ok    [Page.captureScreenshot] /tmp/ocb-audit/shot-example.png 11598 byte
ok    [Target.closeTarget] 1

real  0m13,801s
```

13,8 detik untuk: buka websocket, buat tab, navigate `https://example.com`, baca judul, ambil screenshot PNG 11.598 byte.

> **KOREKSI (21:40, dari `TRACE-SKRIP 141126-2140.md` §8): angka 13,8 detik ini tidak boleh dibaca sebagai latensi.** Smoke test menunggu `document.readyState === 'complete'`, dan `document.readyState` **selalu `undefined`** di mesin ini (nol kemunculan di `js.rs`). Jadi loop penantiannya selalu menghabiskan 100 x 100 ms = 10 detik penuh, tiap jalan. Bukti: navigasi ke halaman **lokal** butuh `real 0m10,996s` lewat smoke test, tapi `Page.navigate` yang sama hanya **180 ms** saat diukur langsung. Angka yang benar: **navigasi lokal ~180 ms**; untuk `https://example.com` sisanya latensi jaringan + parse, dan angka persisnya belum dipisah.

Jejak startup daemon yang relevan (`/tmp/ocb-audit/daemon.log`):

```
INFO store siap db=/tmp/ocb-audit/store.db
INFO gerbang jaringan fetch siap izin_host=["127.0.0.1"]
INFO menyalakan mesin mesin=native kind=rust-native port=0
INFO mesin siap http=http://127.0.0.1:36523 pss_mb=?
INFO penanam siap: runtime mata-tangan disuntik ke setiap tab
INFO port kontrol siap control_bind=127.0.0.1:9223
INFO browserd siap bind=127.0.0.1:9250 control_bind=127.0.0.1:9223
```

Dua catatan dari baris log ini, keduanya belum ditutup:

1. `pss_mb=?` → pengukuran RAM per mesin **belum terpasang** untuk mesin native. Semua klaim "ringan" saat ini bersandar pada angka eksternal, bukan pada laporan daemon sendiri.
2. `WARN reap ditunda; pid mesin tidak terbaca` muncul tiap 5 detik sejak menit pertama. Mesin native tidak punya PID, jadi loop reap tidak bisa bekerja dan hanya menulis baris WARN selamanya. Bukan kebocoran, tapi log yang berisik akan menenggelamkan WARN yang penting.

---

## 4. Temuan P0: `Page.navigate` tidak melewati `KebijakanJaringan`

### 4.1 Klaim desain vs kenyataan

Komentar di `crates/browserd/src/daemon.rs:68-72` menyatakan, verbatim:

> Gerbang jaringan untuk `fetch()` di dalam isolat mesin native. Dibangun di sini, sekali, lalu dipegang setiap tab: **skrip halaman tidak punya jalur jaringan sendiri, dan satu-satunya jalan keluarnya adalah kebijakan ini.**

Kalimat terakhir itu benar untuk skrip, dan **salah untuk dokumen**. Buktinya di kode:

| Jalur | Fungsi | Kebijakan diperiksa? |
|---|---|---|
| `fetch()` dari skrip halaman | `ambil.rs:243` `PengambilJaringan` → `ambil.rs:292` + `alamat_internal` (`ambil.rs:212`) | **Ya** |
| `Page.navigate` / `Target.createTarget` | `mod.rs:529` `ambil_dokumen` → `jaringan.rs:180` `muat` | **Tidak** |

`jaringan::muat` menerima hanya `(url, batas)`. Ia tidak punya parameter kebijakan, tidak punya daftar izin, dan tidak memanggil `alamat_internal`. `KebijakanJaringan` di seluruh repo hanya muncul di `ambil.rs` (definisi + pemakaian skrip), `browserd/src/daemon.rs:75`, dan `browserd/src/config.rs:58`.

### 4.2 Eksperimen diferensial (bukan pembacaan kode)

Daemon dinyalakan dengan `izin_host = ["contoh.invalid"]`, yaitu **hanya** itu yang diizinkan. Alamat internal tidak ada di daftar. Lalu dua permintaan ke alamat internal yang **sama** dibandingkan:

```
=== A. Page.navigate ke http://127.0.0.1:38105/ (izin_host = contoh.invalid) ===
  hasil : {"receipt":{"status":"Changed","sesudah":{"url":"http://127.0.0.1:38105/"}},
           "sumber":{"status":404,"contentType":"","urlFinal":"http://127.0.0.1:38105/"},
           "skrip":{"dijalankan":0}}

=== B. fetch() dari skrip halaman ke alamat yang sama ===
  hasil : {"code":-32000,"message":"skrip gagal: skrip JavaScript gagal: Exception"}

total ms: 648
```

Dua hal yang dibuktikan sekaligus oleh satu eksperimen:

- **B membuktikan gerbangnya hidup.** Skrip ditolak ke alamat yang tidak ada di `izin_host`. Jadi kegagalan A bukan karena kebijakan tidak terpasang, dan bukan karena daemon salah konfigurasi.
- **A membuktikan gerbangnya dilewati.** `Page.navigate` ke alamat yang sama berhasil: `status: 404` adalah jawaban dari server internal `127.0.0.1:38105` (HTTP engine browserd sendiri). Request-nya benar-benar keluar dan jawabannya benar-benar dibaca.

Diulang ke port internal kedua (`127.0.0.1:11500`, layanan embed lokal): hasil identik, `status: 404` dari server itu, skrip tetap ditolak. Jadi bukan kebetulan satu port.

### 4.3 Dampak

Siapa pun yang bisa mengirim `Page.navigate` ke daemon ini (default `127.0.0.1:9224`/`9250`, **tanpa autentikasi**) bisa membuat browserd mengambil:

- `http://127.0.0.1:<port>/…` untuk setiap layanan lokal yang bind loopback: port kontrol `9223`, HTTP engine, layanan lain di host,
- `http://169.254.169.254/latest/meta-data/…` pada host cloud (metadata instance, kredensial IAM),
- `http://10.x` / `172.16.x` / `192.168.x` untuk apa pun di jaringan internal.

Isi jawabannya masuk ke pohon DOM, dan dari situ bisa dibaca lewat `Runtime.evaluate`. Artinya: **bukan cuma SSRF buta, tapi SSRF yang bisa dibaca hasilnya.** Untuk `169.254.169.254` di VPS, itu jalur langsung ke kredensial.

### 4.4 Yang belum diuji (jangan diklaim)

- **Belum diuji** ke `169.254.169.254` sungguhan: mesin ini laptop tanpa metadata service, jadi percobaan hanya akan membuktikan timeout, bukan penerimaan. Yang perlu diuji adalah di VPS dengan metadata aktif.
- **Belum diuji** apakah `Target.createTarget({url})` juga bypass: `ambil_untuk_frame` (`mod.rs:619`) memperlakukan `Target.createTarget` sama dengan `Page.navigate`, jadi secara kode ya, tapi belum dijalankan.
- **Belum diuji** jalur `Screenshot`/`iframe` ke alamat internal (iframe memakai `ambil_bingkai` yang juga memanggil `jaringan::muat` → **dugaan saya ikut bypass**, belum dibuktikan).

### 4.5 Arah perbaikan (belum dikerjakan)

Satu tempat, bukan lima: `jaringan::muat` harus menerima `&KebijakanJaringan` dan menolak sebelum socket dibuka, sama seperti `PengambilJaringan`. Dokumen dan skrip lalu memakai gerbang yang sama. Untuk kasus "klien CDP lokal ingin membuka `localhost` untuk pengembangan", jawabannya bukan melonggarkan default, tapi izin eksplisit di `[fetch] izin_host` yang memang sudah ada.

---

## 5. Catatan provenance

- Binari debug 20:59 dibangun dari sumber yang **sama** dengan `HEAD` sekarang: `find crates xtask -name '*.rs' -newermt '2026-11-14 20:59'` → **0 berkas**. Commit `ba65cc7` (21:18) hanya menyentuh `docs/prd`. Jadi perilaku yang diukur di dokumen ini berlaku untuk kode `HEAD`, bukan untuk snapshot lama.
- Binari rilis terakhir (16:47) **tidak** dipakai untuk pengukuran apa pun di sini: ia lebih tua dari 0 dari sekian perubahan dan tidak memuat `insertAdjacentHTML`.
- Repo bersih kecuali dua berkas tak terlacak: `docs/prd/KONTEKS-BROWSER-RINGAN 141126-2123.md` (milik sesi ini) dan `docs/prd/PRD-MESIN-BUKA-WEB 041126-0631.md.bak44` (berkas asing, **jangan** di-stage).

## 6. Cara mereproduksi

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /mnt/hdd/OpenCrabs/projects/opencrabs/opencrabs-browser

# 1. daemon dengan kebijakan ketat
cat > /tmp/ocb-audit/browserd2.toml <<'EOF'
[daemon]
bind = "127.0.0.1:9250"
store_db = "/tmp/ocb-audit/store2.db"
[engines.native]
kind = "rust-native"
[fetch]
izin_host = ["contoh.invalid"]
EOF
setsid /mnt/hdd/OpenCrabs/build/cargo/debug/browserd serve \
  --bind 127.0.0.1:9250 --config /tmp/ocb-audit/browserd2.toml \
  > /tmp/ocb-audit/daemon2.log 2>&1 < /dev/null &

# 2. navigasi normal (rantai penuh)
node bench/cdp-smoke.mjs --endpoint http://127.0.0.1:9250 --url https://example.com

# 3. eksperimen diferensial SSRF
node /tmp/ocb-audit/ssrf.mjs http://127.0.0.1:<port-internal-dari-log>/
```

---

## 7. Bukti ulang mandiri (dijalankan ulang di turn yang sama, 21:35-21:41 WIB)

Eksperimen §4.2 diulang dengan angka yang bisa dibandingkan langsung, pada daemon `:9250` yang `izin_host = ["contoh.invalid"]` (loopback **tidak** diizinkan):

```
Page.navigate http://127.0.0.1:8099/tes.html
  -> result.sumber  = {status: 200, contentType: "text/html", charset: "utf-8", byte: 531,
                       urlFinal: "http://127.0.0.1:8099/tes.html"}
  -> result.receipt = {status: "NoObservableChange", sesudah: {judul: "Tes Trace", node: 23}}

Runtime.evaluate  try{fetch('http://127.0.0.1:8099/tes.html').status}catch(e){e.message}
  -> "ERR:alamat '127.0.0.1' menunjuk ke jaringan internal host ini; tulis alamat itu
           sendiri di [fetch] izin_host kalau memang itu yang dimaksud"
```

Ini bukan lagi "kode kelihatan melewati gerbang": **531 byte badan dari port loopback benar-benar masuk ke pohon DOM** (`node: 23`), sementara `fetch()` ke URL yang **sama persis**, di daemon yang **sama**, ditolak dengan pesan gerbang. Gerbangnya hidup, dan `Page.navigate` berjalan di sebelahnya.

Tiga titik gagal yang ikut terukur di run ini:

| Permintaan | Hasil | Waktu | Sumber kode |
|---|---|---|---|
| `nav` ke server tanpa listener (`127.0.0.1:1/`) | `-32000` "Connection refused (os error 111)" | 2,1 ms | `jaringan.rs` (ureq connect) |
| `nav` ke `application/json` (`/json/version`) | `-32000` "hanya mengurai dokumen HTML" | 3,6 ms | `mod.rs` cek content-type |
| `nav` ke 404 | **sukses**, `judul: "Error response"`, DOM diganti | 41,5 ms | `state.rs:navigasi` |
| `Bogus.method` | `-32601` | 1,3 ms | `state.rs` dispatch |
