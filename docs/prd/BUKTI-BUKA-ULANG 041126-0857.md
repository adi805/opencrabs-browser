# Bukti buka ulang situs yang pernah bermasalah

Diambil 4 Nov 2026 08:54-08:58 WIB lewat daemon `browserd` di `127.0.0.1:9224`
(tab baru per situs, `Page.enable` + `Network.enable` + `Runtime.evaluate` +
`Page.captureScreenshot`, tab ditutup sesudahnya). Port produksi 9222 dibaca
read-only sebelum dan sesudah: 0 tab, tidak menerima perintah apa pun.

Situs yang dulu gagal adalah halaman consent Google: `browser_click` dan
`browser_act` time out, `browser_eval` + `.click()` berhasil, dan
`querySelectorAll('[role="checkbox"]')` mengembalikan 0 karena kotaknya custom
element. Itu sebabnya halaman ini diuji ulang lebih dulu.

## Hasil

| Situs | Status dokumen | URL akhir | Judul | Sinyal DOM | Detik | Kategori yang benar |
|---|---|---|---|---|---|---|
| `google.com/search?q=...` | 200 lalu **429** | `/sorry/index?...` | kosong | frame `recaptcha/enterprise` | 25,1 (mentok batas) | `blok_anti_bot` |
| `nowsecure.nl` | tak tertangkap (tab dibuat sudah membawa URL) | `nowsecure.nl` | `nowsecure.nl` | `captcha_dom` (hCaptcha) | 2,0 dan 13,3 di dua run | `blok_anti_bot` |
| `tidak-ada-host-oc.test` | tidak ada | `chrome-error://chromewebdata/` | kosong | tidak ada | 0,3 | `jaringan` |
| `instagram.com` | tidak tertangkap | `instagram.com` | `Instagram` | tidak ada | 15,2 | `render_lambat` (di ambang) atau sukses |
| `consent.google.com/?gl=ID` | **404** | sama | `Error 404 (Tidak Ditemukan)!!1` | tidak ada | 0,9 | bukan tantangan, halaman galat biasa |

## Tiga pelajaran yang mengubah desain klasifikasi

1. **Status sendirian tidak cukup, dan status bisa hilang.** Klien yang membuat
   tab lewat `PUT /json/new?<url>` memulai navigasi sebelum `Network.enable`
   sempat dikirim, jadi `Network.responseReceived` untuk dokumen pertama tidak
   pernah terlihat. `nowsecure.nl` membuktikannya: nol event dokumen, tapi
   halamannya jelas tantangan. Klasifikasi wajib punya jalur dari URL akhir dan
   sinyal DOM, bukan hanya status.
2. **Google membalas 429 di URL `/sorry/index`, bukan 403, dan teks halamannya
   0 karakter.** Deteksi berbasis teks saja akan melaporkan "halaman normal".
   Bukti yang menyelamatkan: status 429 + pola URL + frame reCAPTCHA.
3. **`chrome-error://chromewebdata/` adalah bukti jaringan yang paling kuat.**
   Halaman galat Chromium punya body 0 karakter, jadi tanpa memeriksa URL akhir
   sistem akan menyebutnya "halaman kosong" dan bukan kegagalan jaringan.

## Yang belum diukur

Laju deteksi anti-bot pada host nyata (satu percobaan per situs, bukan sampel),
dan perilaku `chrome_full` di xvfb. Tidak ada klaim soal keduanya.

Berkas mentah: `/tmp/oc-buka-{google2,cf2,dnsgagal,ig,consent}.json` dan
screenshot pasangannya.