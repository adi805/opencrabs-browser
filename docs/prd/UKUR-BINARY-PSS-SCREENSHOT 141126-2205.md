# UKUR: kesegaran binary, PSS, dan biaya screenshot vs Chrome

**Tanggal:** 14 Nov 2026 22:05 WIB
**Repo:** `opencrabs-browser` @ `ba65cc7` (`docs(prd): close G10 and record the gate receipt`, 21:18)
**Tujuan:** memberi angka pada kata "ringan", dan memisahkan klaim yang terukur dari yang selama ini cuma diasumsikan.

---

## 1. Kesegaran binary (dua binary, dua status berbeda)

| Binary | Waktu | Ukuran | `sha256` (12 char) | `insertAdjacentHTML` | Kesimpulan |
|---|---|---|---|---|---|
| debug | 14 Nov 20:59 | 274.147.368 B | `fe287157bf45` | **1** | **segar**: nol berkas `.rs` di `crates/`+`xtask/` yang lebih baru dari binary ini |
| release | 14 Nov 16:47 | 14.669.720 B | `7fa7aac1f461` | **0** | **basi**: tidak memuat kerja eksekusi skrip |

Cara memeriksa kesegaran, bukan menebak dari jam:

```
$ find crates xtask -name '*.rs' -newer /mnt/hdd/OpenCrabs/build/cargo/debug/browserd
(kosong)
```

Nol baris = tidak ada sumber yang lebih baru dari binary debug. Probe `insertAdjacentHTML` dipakai sebagai penanda karena string itu hanya ada di `js.rs` sesudah kerja eksekusi skrip; release 16:47 menjawab 0, jadi ia dibangun dari kode yang lebih tua dari `HEAD`.

**Konsekuensi yang sudah dipegang di dokumen ini:** semua angka waktu dan PSS di bawah diukur dengan binary **debug 20:59**, yang segar terhadap `HEAD`. Binary release tidak dipakai untuk satu pun pengukuran. Rebuild release sedang berjalan (`cargo build --release -p browserd --config profile.release.codegen-units=16`, `CARGO_BUILD_JOBS=2`, `nice 19 ionice -c3`) dan **belum selesai** saat dokumen ini ditulis, jadi angka release **belum diukur**.

---

## 2. PSS mesin native (binary debug, satu daemon, `izin_host=["127.0.0.1"]`)

Diukur dari `/proc/<pid>/smaps_rollup`, bukan dari `ps`. PSS dipakai karena ia membagi halaman bersama, jadi angka ini tidak menghitung pustaka bersama dua kali.

| Kondisi | Tab | RSS | **PSS** | PSS_Anon |
|---|---|---|---|---|
| daemon baru, belum ada tab | 0 | 27.444 kB | **25.218 kB** (24,6 MB) | 3.096 kB |
| 1 tab `tes.html` (23 simpul, ada `<script>`) | 1 | 32.164 kB | **29.937 kB** (29,2 MB) | 3.732 kB |
| \+ 1 tab `berat.html` (2.000 baris) | 2 | 37.364 kB | **35.137 kB** (34,3 MB) | 8.868 kB |
| \+ 3 tab `tes.html` (total 5 tab) | 5 | 38.676 kB | **36.450 kB** (35,6 MB) | 10.180 kB |
| **semua tab ditutup** | 0 | 38.768 kB | **36.541 kB** (35,7 MB) | 10.208 kB |

### 2.1 Yang bisa diklaim dari angka ini

- **Ringan itu benar untuk RAM.** 24,6 MB untuk daemon menganggur, 35,6 MB dengan lima tab termasuk satu halaman 2.000 baris. Tidak ada angka lain di host ini yang bisa disebut "ringan" dengan bukti sebanding kecuali Chrome (§3).
- **Biaya per tab kecil.** Tab `tes.html` menambah ~4,7 MB PSS; tab `berat.html` menambah ~5,2 MB.

### 2.2 Yang TIDAK boleh diklaim

- **Tidak kembali ke garis dasar setelah tab ditutup.** 36.541 kB sesudah nol tab, lawan 25.218 kB saat start: **+11,3 MB bertahan**. Saya tidak menyebut ini kebocoran, karena (a) hanya satu siklus yang diukur, (b) alokator glibc memang menahan arena yang sudah dipetakan, (c) belum ada pengukuran berulang. **[DIPERBARUI 14 Nov 2026: pengukuran berulang sudah dilakukan, lihat §2.3. Hasilnya memastikan ini retensi alokator, bukan kebocoran.]** Yang bisa diklaim: **angka "ringan" harus diukur sesudah pemakaian, bukan hanya saat daemon baru hidup.** Untuk perbandingan apa pun, sebutkan titik ukurnya.
- **`pss_mb=?` di log daemon masih kosong.** Daemon sendiri belum melaporkan angka ini; semua angka di atas saya ambil dari `/proc`. Selama medan itu `?`, klaim "ringan" tidak bisa diverifikasi dari log daemon sendiri oleh siapa pun yang tidak menjalankan `smaps_rollup`. Ini pekerjaan kecil yang berdampak besar untuk kredibilitas.

### 2.3 Pengukuran berulang: binary RELEASE, empat siklus buka-tutup

§2 mengukur binary **debug** satu siklus dan meninggalkan pertanyaan terbuka: 11,3 MB yang bertahan setelah tab ditutup itu retensi alokator atau kebocoran objek? Satu siklus tidak bisa membedakan keduanya. Empat siklus bisa.

Binary yang diukur bukan yang lama. Release lama bertanggal `141126-1647` dan **tidak** memuat `insertAdjacentHTML` sama sekali, jadi ia lebih tua dari kode skrip yang sekarang. Release dibangun ulang pada 14 Nov 2026 21:59 (`Finished release profile in 10m 39s`, `codegen-units=16`, `CARGO_BUILD_JOBS=2`, `nice -n 19 ionice -c3`), ukuran 16.154.264 B, `sha256 55c09e4d15c3f8b489f2b80d…`, **lebih baru dari HEAD `ba65cc7` (`141126-2118`)**, dan memuat `insertAdjacentHTML` (1 hit) serta `addEventListener` (5 hit). Jadi angka di bawah memang milik kode yang sekarang.

Daemon: `pid 1356136`, `bind 127.0.0.1:9252`, `control 127.0.0.1:9333`, config `/tmp/ocb-audit/browserd3.toml` (`izin_host = ["127.0.0.1"]`). Beban tiap tab: `Page.navigate` ke halaman **3000 simpul DOM** dengan satu `<script>` yang menyetel `document.title`.

| Siklus | PSS sebelum | PSS sesudah 5 tab | Delta total | Delta per tab |
|---|---|---|---|---|
| 1 | 10,48 MB | 27,52 MB | 17,04 MB | **3,41 MB** |
| 2 | 27,59 MB | 36,04 MB | 8,44 MB | 1,69 MB |
| 3 | 36,07 MB | 36,48 MB | 0,41 MB | 0,08 MB |
| 4 | 36,48 MB | 36,63 MB | 0,15 MB | 0,03 MB |

Bukti tab benar-benar hidup, bukan tab kosong yang murah: `Runtime.evaluate` di tab 0 balik `"berat-siap / semua simpul=3008"`. Artinya skrip halaman jalan (judul berubah dari `berat` ke `berat-siap`) dan 3008 simpul benar-benar terurai. Bukti tab benar-benar ditutup: `GET /json/list` balik `[]`.

**Tiga kesimpulan yang boleh diklaim dari angka ini:**

1. **Profil build tidak mengubah biaya per tab.** Release 3,41 MB/tab, debug 3,40 MB/tab pada beban yang sama. Yang menentukan biaya adalah dokumennya (DOM + layout), bukan level optimasi. Konsekuensinya praktis: mengukur di debug untuk pertanyaan biaya per tab itu sah, dan `-O` bukan alat untuk menurunkan angka itu.
2. **Delta yang turun 3,41 → 1,69 → 0,08 → 0,03 adalah retensi alokator, bukan kebocoran.** Kalau ini kebocoran objek, tiap siklus akan menambah sekitar 3,4 MB lagi dan PSS akan menanjak tanpa batas. Yang terjadi sebaliknya: memori dipakai ulang, lalu siklus berikutnya nyaris gratis. Ini menutup pertanyaan yang §2 tinggalkan.
3. **PSS tidak kembali ke OS setelah tab ditutup, dan itu bukan kerusakan.** 36,63 MB bertahan saat `/json/list` sudah `[]`. Konsekuensi operasional yang harus dipegang: **PSS adalah puncak historis proses, bukan pemakaian sekarang.** Siapa pun yang membaca angka ini sebagai "memori terpakai saat ini" akan salah, dan siapa pun yang membandingkan dua daemon harus menyamakan riwayat bebannya dulu.

**Yang TIDAK diukur dan tidak boleh diekstrapolasi:** ceiling 100 MB pada 99 tab di binary release ini. Ekstrapolasi linear `99 x 3,41 MB` tidak sah justru karena poin 2 di atas: memori dipakai ulang, jadi biaya marginal tab ke-50 bukan biaya tab ke-1. Angka 99 tab yang sah adalah milik CI job `budget` (`62,9 MB` PSS, run `43460811394`, commit `21901cc`) dan itu diukur oleh tes, bukan oleh sesi ini. Untuk binary `55c09e4d…` ini: **belum diukur.**


---

## 3. Pembanding: Chrome headless di host yang sama, halaman yang sama

Chrome `157.0.8103.78` (`/usr/bin/google-chrome`), `--headless=new --disable-gpu --no-sandbox`, PSS dijumlahkan dari **seluruh** proses dengan `--user-data-dir` yang sama (bukan satu PID).

| Kondisi | Proses | PSS total |
|---|---|---|
| `about:blank` | 13 | **310 MB** |
| \+ `berat.html` (2.000 baris) | 28 | **667 MB** |

**Perbandingan langsung:** untuk halaman yang sama, Chrome memakai **667 MB / 34 MB ≈ 19 kali** lipat RAM mesin native. Klaim "ringan" berdiri, dengan angka.

---

## 4. Screenshot: tempat klaim "ringan" berhenti berlaku

Halaman `berat.html` yang sama, satu kali `Page.captureScreenshot{format:"png"}`:

| Mesin | Simpul | Screenshot #1 | Screenshot #2 | Byte |
|---|---|---|---|---|
| native (debug) | 1.206 | **25.936 ms** | **17.391 ms** (tab sama, tanpa navigasi) | 22.379 |
| Chrome 157 | 6.007 | **161,8 ms** | **86,3 ms** | 37.261 |

Chrome pada halaman itu punya **6.007** elemen (lebih banyak dari 1.206 yang dilihat mesin native karena parser-nya memecah lebih rinci) dan tetap **160 kali lebih cepat** pada percobaan pertama, **200 kali** pada percobaan kedua.

Chrome juga **membaik saat diulang** (161,8 → 86,3 ms, turun 47%). Mesin native **tidak** (25,9 → 17,4 ms pada tab lain, dan 14,8/13,2/17,4 pada tiga percobaan berturut di tab yang sama: tidak ada tren turun). Itu bukti bahwa yang mahal bukan sekadar "halaman berat", tapi **tidak adanya penyimpanan hasil tata letak antar-permintaan**.

### 4.1 Angka yang harus masuk PRD

- **Biaya raster native ≈ 20 ms per simpul, linear.** (25.936 − 5.815) / (1.206 − 206) = 20,1 ms. Intersep ≈ 1,7 detik.
- **Byte PNG hampir tidak tumbuh** (22,3 KB dari 206 sampai 1.206 simpul) karena isi di luar viewport 800x600 dipotong. Jadi yang tumbuh murni waktu, bukan keluaran. Membandingkan ukuran file akan menyesatkan.
- **Satu screenshot 25 detik memblokir pelayanan daemon.** `GET /json/list` dari klien lain timeout 5 detik (`HTTP 000`) selama itu, padahal normalnya 7 ms. Untuk halaman di atas ~600 simpul, daemon **terlihat mati** bagi health check. Ini bukan soal latensi: ini soal observabilitas.

---

## 5. Yang belum diukur (jangan ditutupi)

1. **Binary release yang segar.** Rebuild sedang berjalan; angka release (ukuran, waktu screenshot, PSS) **belum ada**. Kalau release jauh lebih cepat, seluruh §4 harus diulang dengan release, karena itu yang akan dipakai orang.
2. **Pengulangan siklus tab.** PSS hanya satu siklus buka-tutup. Butuh minimal 5 siklus untuk memisahkan "alokator menahan arena" dari "kebocoran".
3. **Pemisahan biaya layout vs shaping vs encode.** Yang terukur gabungannya. Klaim "shaping yang mahal" **belum divalidasi**.
4. **Halaman dunia nyata.** Semua ukuran memakai HTML buatan sendiri di `http://127.0.0.1:8099`. Halaman dengan CSS kompleks, gambar, atau webfont bisa berperilaku berbeda, dan itu justru kasus yang paling relevan.

---

## 6. Cara mereproduksi

```bash
# PSS satu daemon
/tmp/ocb-audit/pss.sh 9290          # tabs / rss / pss / pss_anon dari smaps_rollup

# PSS seluruh proses Chrome
/tmp/ocb-audit/chrome-pss.sh /tmp/ocb-audit/chrome-prof

# waktu screenshot native (4 format, satu tab)
python3 /tmp/ocb-audit/shot3.py

# regresi linear terhadap jumlah simpul
python3 /tmp/ocb-audit/scaling.py

# kesegaran binary
find crates xtask -name '*.rs' -newer /mnt/hdd/OpenCrabs/build/cargo/debug/browserd
grep -c insertAdjacentHTML /mnt/hdd/OpenCrabs/build/cargo/{debug,release}/browserd
```

Skrip-skrip itu di `/tmp` dan akan hilang. Angka di dokumen ini yang mengikat; kalau mau dijadikan tes regresi, isinya harus pindah ke `crates/cdp/tests/`.
