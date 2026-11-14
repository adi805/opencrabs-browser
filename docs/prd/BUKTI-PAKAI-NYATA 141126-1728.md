---
document_type: verification-report
project: "OpenCrabs Browser: web-opening engine"
prd: "PRD-MESIN-RUST 071126-1413.md"
status: complete
measured_at: "2026-11-14 17:13-17:28 WIB"
engine: "RustNativeEngine/0.2.0"
binary: "/mnt/hdd/OpenCrabs/build/cargo/release/browserd (14 669 720 byte, 2026-11-14 16:47)"
---

# Uji Pakai Nyata: Mesin Native Bisa Dipakai atau Tidak

Pertanyaan yang dijawab berkas ini: **browser buatan sendiri sudah selesai dan
bisa dipakai?** Bukan "tes lulus", bukan "gate hijau". Yang dipakai adalah
binary release sungguhan di port terpisah, situs sungguhan lewat jaringan
sungguhan, dan hasilnya dibaca dengan mata (vision) bukan dari exit code.

## Ringkasan putusan

| Pertanyaan | Jawaban | Dasar |
|---|---|---|
| Bisa membuka situs nyata? | **Ya** | `https://example.com` 342 ms, judul + DOM 16 node + teks benar |
| Bisa render dan screenshot? | **Ya** | PNG 800x600 11 598 byte, magic `89504e47`, teks terbaca oleh vision |
| Ringan seperti klaim? | **Ya, terukur** | PSS 18.1 MB (1 tab), 54.4 MB (10 tab situs nyata), nol proses anak |
| 100% Rust tanpa Chrome? | **Ya** | `pgrep -P <daemon>` = 0, nol proses `chrome` ber-PPid daemon |
| Full fitur? | **TIDAK** | Lihat tabel celah di bawah: JS halaman tidak pernah dieksekusi |

## 1. Bukti yang lolos

Daemon dijalankan dari binary release di `127.0.0.1:9250` dengan store
terisolasi (`/tmp/ocb-live-test/`), `[engines.native] kind = "rust-native"`.

```
{"Browser":"RustNativeEngine/0.2.0","Protocol-Version":"1.3",
 "User-Agent":"Mozilla/5.0 (X11; Linux x86_64) RustNativeEngine/0.2.0",
 "webSocketDebuggerUrl":"ws://127.0.0.1:9250/devtools/browser/..."}
```

`bench/cdp-smoke.mjs` → 6/6 `SEMUA LOLOS`. Navigasi nyata:

| Ukuran | Hasil |
|---|---|
| `Page.navigate` → `https://example.com` | 342 ms, `errorText` none |
| `document.title` | `"Example Domain"` |
| `DOM.getDocument depth=-1` | 16 node |
| `document.body.textContent` | teks lengkap, benar |
| `Page.captureScreenshot` | PNG 800x600 RGBA, 11 598 byte |
| Event CDP | `Page.loadEventFired`, `Runtime.executionContextCreated` |

Screenshot diperiksa dengan vision, bukan diasumsikan: teks terbaca, centered,
hitam di atas putih.

### Mutasi DOM lewat CDP terlihat di raster

Ini yang membuktikan DOM, layout, dan raster memakai pohon yang sama:

```
Runtime.evaluate: h1.textContent = 'DIUBAH DARI CDP'
                  + appendChild(<p style="color:#c00;font-size:28px">)
                  + body style padding:40px; font:20px sans-serif
→ screenshot 8 533 byte
→ vision: "DIUBAH DARI CDP" (hitam), "paragraf baru dari otomasi" (MERAH)
```

Warna merah terbaca, artinya `style` inline dihormati sampai ke raster, bukan
cuma diparse.

## 2. Memori: klaim "ringan" terukur

Dibaca dari `/proc/<pid>/smaps_rollup` milik daemon, bukan perkiraan:

| Kondisi | PSS | Rss | Anak proses |
|---|---|---|---|
| Idle + 1 halaman live | 18.1 MB | 20.7 MB | 0 |
| 10 tab situs nyata | 54.4 MB | 56.9 MB | 0 |

Beban 10 tab: `toolkuy.com` 1 238 node, `rust-lang.org` 633, `iana.org` 287,
`example.com` 16. Sepuluh tab berhasil, nol gagal, 29.9 detik.

Bandingkan dengan jalur Chrome: satu proses Chrome plus renderer per tab. Di
sini **nol** proses anak. Itu bukan optimasi, itu perbedaan arsitektur.

## 3. Celah yang TIDAK tercatat di `CELAH`

`engine::native::CELAH` punya 12 entri dan semuanya jujur soal batas yang
dinyatakan. Yang di bawah ini **tidak ada di daftar itu** dan baru ketemu
karena uji live, bukan karena membaca dokumen.

| Yang gagal di uji live | Status di `CELAH` | Dampak nyata |
|---|---|---|
| `<script>` halaman **tidak pernah dieksekusi** | **tidak disebut sama sekali** | Situs modern tampil kosong: isi dirender oleh JS. Ini celah paling besar |
| `typeof location` = `undefined` | tidak disebut | Skrip halaman yang membaca `location.href` langsung throw |
| `window.innerWidth/innerHeight` = `undefined` | tidak disebut | Skrip responsif tidak bisa membaca viewport |
| `getComputedStyle` = `undefined` | tidak disebut | Otomasi yang membaca gaya terhitung gagal |
| `getBoundingClientRect` tidak ada | tidak disebut | Pengukuran posisi elemen dari skrip tidak mungkin |
| `document.body.innerText` throw | tidak disebut | `innerText` umum dipakai untuk ambil teks |
| `innerHTML` getter throw | hanya disebut **setter**-nya | Getter pun tidak ada, padahal CELAH hanya menyebut setter |
| `getElementsByTagName` throw | tidak disebut | API klasik, masih dipakai banyak skrip |
| `firstElementChild`/`lastElementChild` = `null` padahal `children[0]` ada | tidak disebut | Nilai salah tanpa galat: paling berbahaya karena diam |
| `setTimeout`/`addEventListener` = `undefined` | tidak disebut | Tidak ada timer maupun event listener halaman |
| `Element`/`document.createEvent` = `undefined` | tidak disebut | Tidak ada prototipe DOM maupun event sintetis |

### Kenapa ini penting

`CELAH` adalah kontrak kejujuran repo ini: setiap batas yang diketahui wajib
dinyatakan di kode, dan tes gagal kalau entri dihapus tanpa kapabilitasnya ada.
Sebelas API di atas lolos dari kontrak itu. Artinya kontraknya bekerja untuk
hal yang **sudah dipikirkan**, tapi tidak menangkap hal yang **belum
dipikirkan**. Uji live menangkapnya dalam 15 menit.

### Yang lebih buruk dari tidak menyebut: menyiratkan sebaliknya

`CELAH` bukan hanya diam soal eksekusi `<script>`. Ia memakai frasa "skrip
halaman" di empat tempat dengan asumsi skrip halaman **bisa jalan**:

```
"jadi `keydown` hanya bisa dipicu skrip halaman lewat `dispatchEvent`"
"mutasi dari skrip halaman terlihat oleh `DOM.getDocument` dan oleh raster"
"`contentDocument`/`contentWindow` tidak ada di isolat, jadi skrip halaman
 tidak bisa menyentuh dokumen anak"
"`pilih_terang` untuk skrip halaman yang berhenti di batas pohon"
```

Empat kalimat itu masuk akal kalau pembacanya menganggap `<script>` dieksekusi.
Diukur hari ini, tidak ada satu pun jalur di `crates/cdp/src/native_wire/` yang
mengeksekusi `<script>` dari dokumen. `grep -n "script"` di seluruh
`native_wire/` mengembalikan nol kecocokan selain `javascript:` di URL. Jadi
daftar celahnya bukan sekadar tidak lengkap: ia memberi kesan kapabilitas yang
tidak ada, dan itu kelas kesalahan yang sama dengan mengklaim tes yang tidak
pernah ditulis.

Satu tes juga menyesatkan namanya: `native::js::tests::test_js_runtime_dom_mutations`
diberi komentar "AC-003. Skrip halaman harus bisa ..." tetapi isinya memanggil
`Runtime.evaluate` dari luar. Skrip halaman yang sesungguhnya (`<script>` di
dalam dokumen) tidak pernah dijalankan oleh tes mana pun. Namanya benar untuk
"mutasi DOM dari CDP", salah kalau dibaca sebagai "skrip halaman jalan".

## 4. Putusan: bisa dipakai untuk apa, belum untuk apa

**Sudah bisa dipakai hari ini, tanpa Chrome:**

- membuka URL sungguhan lewat `http`/`https`, termasuk redirect, dan melaporkan
  URL akhir
- membaca DOM (16 sampai 1 238 node pada situs nyata), teks, judul, atribut
- menyeleksi dengan CSS selector, termasuk menembus shadow root terbuka
- memutasi DOM dari otomasi (`Runtime.evaluate`), dan mutasi itu **terlihat di
  raster** termasuk warna dan ukuran font
- screenshot PNG/JPEG/WebP pada 800x600
- pohon aksesibilitas (`Accessibility.getFullAXTree`)
- 10 tab bersamaan dengan 54.4 MB PSS dan nol proses anak
- memori per tab yang jauh di bawah jalur Chrome, tanpa satu pun proses
  `chrome` atau `chrome-headless-shell`

**Belum bisa dipakai untuk:**

- **situs modern apa pun yang mengandalkan JavaScript untuk merender isinya.**
  Ini bukan kasus tepi. Hampir semua aplikasi web hari ini mengirim HTML kosong
  lalu mengisinya dengan `fetch` + `innerHTML`. Mesin ini akan menampilkan
  halaman kosong, dan tidak ada galat yang memberi tahu pemanggil bahwa isinya
  hilang.
- login, captcha, form submission yang butuh handler `onsubmit`
- apa pun yang memakai `setTimeout`, `addEventListener`, atau `location`

Batasnya bukan "kurang beberapa API". Batasnya adalah **tidak ada eksekusi
skrip halaman sama sekali**, dan itu satu tingkat di bawah "DOM belum lengkap".

## 5. Langkah berikutnya, berurutan

| # | Kerjaan | Kenapa ini dulu |
|---|---|---|
| 1 | Jalankan `<script>` saat parse selesai (inline dulu, lalu `src`) di isolat yang sama dengan DOM | Tanpa ini, situs modern tampil kosong. Semua celah lain jadi tidak relevan |
| 2 | Tambah `location`, `window.innerWidth/innerHeight`, `setTimeout`, `addEventListener` | Skrip halaman langsung throw tanpa ini, jadi nomor 1 tidak ada artinya |
| 3 | `getComputedStyle`, `getBoundingClientRect`, `innerText`, `innerHTML` getter, `getElementsByTagName`, `firstElementChild`/`lastElementChild` | Otomasi dan skrip umum memakainya; `firstElementChild` yang balik `null` diam-diam paling berbahaya |
| 4 | Tulis ulang entri `CELAH` supaya menyebut butir 1-3 secara eksplisit, dan ganti frasa "skrip halaman" yang menyiratkan sudah jalan | Kontrak kejujuran repo ini rusak kalau daftarnya menyiratkan kapabilitas yang tidak ada |
| 5 | Ganti nama atau perbaiki komentar `test_js_runtime_dom_mutations` supaya tidak dibaca sebagai bukti eksekusi `<script>` | Tes yang namanya lebih luas dari isinya adalah klaim palsu yang lolos gate |

Nomor 1 dan 2 adalah satu paket: menjalankan `<script>` tanpa `setTimeout` dan
`location` hanya memindahkan kegagalan, bukan menghilangkannya.

## 6. Cara mengulang seluruh uji ini

```bash
# 1. daemon dari binary release, store terisolasi
D=/tmp/ocb-live-test && mkdir -p $D
/mnt/hdd/OpenCrabs/build/cargo/release/browserd keygen \
  --key-file $D/store.key --db $D/store.db
printf 'bind = "127.0.0.1:9250"\ncontrol_bind = "127.0.0.1:9251"\nidle_reap_secs = 0\nmax_tabs = 20\n\n[engines.native]\nkind = "rust-native"\nmax_tabs = 20\n' > $D/browserd.toml
setsid nohup /mnt/hdd/OpenCrabs/build/cargo/release/browserd serve \
  --bind 127.0.0.1:9250 --config $D/browserd.toml > $D/daemon.log 2>&1 </dev/null &

# 2. bukti protokol
curl -s http://127.0.0.1:9250/json/version     # harus RustNativeEngine/0.2.0
cd ~/src/opencrabs-browser && node bench/cdp-smoke.mjs --endpoint http://127.0.0.1:9250

# 3. memori
grep -E '^(Rss|Pss):' /proc/$(pgrep -f 'browserd serve.*9250')/smaps_rollup
pgrep -P $(pgrep -f 'browserd serve.*9250') | wc -l   # harus 0
```

Skrip probe lengkap ada di `/tmp/ocb-live-test/` (`real-nav.mjs`, `load.mjs`,
`jstest2.mjs`, `mutshot.mjs`, `navscript.mjs`). Semuanya memakai CDP mentah
lewat WebSocket, jadi tidak bergantung pada Puppeteer maupun Playwright.

## 7. Batas laporan ini

- Diukur pada satu host (laptop Lenovo, Linux, 4 core, 7 GB RAM), satu kali
  jalan per skenario kecuali PSS yang diambil pada dua kondisi berbeda.
- Angka PSS adalah RSS/PSS proses daemon saja. Ia **bukan** angka per tab pada
  beban penuh 99 tab; angka itu ada di `BUKTI-PSS-DAEMON 131126-0323.md` dan
  diukur dengan tes khusus, bukan oleh laporan ini.
- Situs yang dipakai: `example.com`, `toolkuy.com`, `iana.org`,
  `rust-lang.org`, plus satu halaman lokal. Nol situs yang butuh login, captcha,
  atau SPA berat. Kalau ada yang mencoba situs seperti itu, hasilnya akan
  berbeda dan lebih buruk, bukan lebih baik.
- Semua angka di berkas ini dibaca dari tool output pada 2026-11-14 pukul
  17:13-17:28 WIB. Tidak ada yang dikutip dari ingatan.
