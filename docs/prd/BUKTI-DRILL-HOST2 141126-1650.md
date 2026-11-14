# BUKTI: Drill Backup Store ke Host Kedua

Tanggal: **2026-11-14**, WIB. Unit 15 dari `RENCANA-TUNTAS`.

Dokumen ini menutup kriteria task 15: *"`browserd cookies ls` di host kedua
melaporkan jumlah cookie bukan nol"*. Drill 2026-11-07
(`BUKTI-RESTORE 071126-2025.md`) sudah menyebut batasannya sendiri: store di
laptop nol baris cookie, jadi yang terbukti cuma paritas byte. Nol cookie yang
dibaca ulang tidak bisa dibedakan dari store rusak yang gagal dengan sopan.

## 1. Kenapa host kedua dan bukan salin lokal

Salin di direktori yang sama tidak membuktikan langkah pulih: berkas yang
dibuka di tempat asalnya bisa saja masih memakai inode yang sama. Yang diuji di
sini adalah perpindahan: berkas menyeberang ke host lain, dipulihkan ke
direktori bersih di sana, lalu dibuka oleh biner di sana.

## 2. Kenapa container terpisah, bukan host tujuan langsung

Owner memutuskan pemeriksaan dijalankan di container Podman sekali pakai, bukan
di ruang pengguna host tujuan. Alasannya bukan kerapian: joyboy adalah host
produksi (Caddy, mail, listmonk, umami, AdGuardHome). Menjalankan biner uji di
sana menaruh berkas dan proses uji di dekat layanan yang hidup.

Karena itu drill memakai `podman run --rm --network none`, dengan hanya
direktori drill yang ter-mount. Jaringan dimatikan: yang diuji adalah kemampuan
membuka store, dan store tidak butuh jaringan. Kalau ada yang mencoba keluar,
tidak ada jalan keluarnya.

## 3. Bahan

| Berkas | Peran |
|---|---|
| `store.db` | SQLite: `profile`, `session`, `cookie`, `otp_event`, `captcha_event`, `engine_run` |
| `store.key` | kunci XChaCha20-Poly1305 32 byte, mode wajib `0600` |
| `browserd` | biner hasil build lokal, `release` |

Cookie yang menyeberang adalah **sintetis**, dibuat `crates/store/examples/seed_drill.rs`.
Alasan: cookie asli adalah token sesi yang masih hidup, dan latihan transport
tidak butuh kredensial nyata.

## 4. Perintah

```bash
# 1. store berisi cookie sintetis, dibuat di laptop
cargo run --release -q -p store --example seed_drill -- <dir> 5

# 2. drill: kirim, pulihkan, buka di container host kedua
bash scripts/backup-drill.sh \
  --host joyboy \
  --src <dir> \
  --bin /mnt/hdd/OpenCrabs/build/cargo/release/browserd \
  --podman docker.io/library/ubuntu:24.04
```

Skrip menolak jalan kalau `store.db` atau `store.key` tidak ada, kalau biner
bukan berkas yang bisa dijalankan, atau kalau mode kunci bukan `0600`.

## 5. Hasil

Diisi dari keluaran skrip di bagian 6. Setiap baris di bawah ini punya pasangan
`LOLOS`/`GAGAL` di log, bukan penilaian sendiri.

| Pemeriksaan | Hasil |
|---|---|
| mode kunci di sumber | `LOLOS 600` |
| sha256 `store.db` terkirim | `47f8c3f3cf9518597971b2c7d78ab632b59a77ab844a677772c08c8947bcc7b8` |
| sha256 `store.db` hasil pulih | identik: `47f8c3f3cf95…cc7b8` |
| sha256 `store.key` hasil pulih | `952ab0be050bb5b8f21c02d657918ac818aedd861f92ec2beb85ab31dbdc7d7e` |
| mode kunci hasil pulih | `LOLOS 600` |
| `cookies ls` di host kedua | **5 cookie**, exit 0 |
| kontrol negatif: kunci salah | ditolak, exit 1 |

Jumlah cookie sumber: `cargo run --release -p store --example seed_drill` mencetak
`ditulis 5 cookie, terbaca kembali 5, dilewati 0`, dan `cookies ls` lokal
mengonfirmasi `5 cookie di profil drill`.

## 6. Keluaran mentah

```
== drill backup store -> joyboy ==
sumber = /mnt/hdd/OpenCrabs/projects/browserd-backup-drill/seed
biner  = /mnt/hdd/OpenCrabs/build/cargo/release/browserd
  LOLOS  mode kunci di sumber: 600
  info   store.db 65536 byte, sha256 47f8c3f3cf95
  info   store.key 65 byte, sha256 952ab0be050b
tujuan = joyboy:/tmp/ocb-backup-drill-yTD0cA
1. terkirim
2. dipulihkan ke direktori bersih
  LOLOS  sha256 store.db terkirim: 47f8c3f3cf9518597971b2c7d78ab632b59a77ab844a677772c08c8947bcc7b8
  LOLOS  sha256 store.db hasil pulih: 47f8c3f3cf9518597971b2c7d78ab632b59a77ab844a677772c08c8947bcc7b8
  LOLOS  sha256 store.key hasil pulih: 952ab0be050bb5b8f21c02d657918ac818aedd861f92ec2beb85ab31dbdc7d7e
3. paritas byte diperiksa
  LOLOS  mode kunci hasil pulih: 600
4. cookies ls di joyboy: 5 cookie (exit 0)
  LOLOS  exit cookies ls: 0
  LOLOS  cookie terbaca di host tujuan bukan nol (5)
5. kontrol negatif: kunci salah ditolak (exit 1)

LOLOS: 5 cookie menyeberang host, dipulihkan, dan terbaca kembali di joyboy
DRILL_EXIT:0
```

Sesudah drill: `podman ps -a` di joyboy nol container `ocb`, `/tmp/ocb-*` nol
direktori, `/home` tetap 30G bebas. Container `--rm` dan trap `BERSIHKAN`
bekerja, tidak ada sisa uji di host produksi.

## 7. Batas yang jujur

- Yang dibuktikan adalah **store** yang selamat menyeberang host. Yang tidak
  dibuktikan: ketersediaan host tujuan, jadwal backup otomatis, dan retensi.
  Drill ini latihan pulih, bukan sistem backup.
- Container memakai image `ubuntu:24.04` polos. Biner `browserd` di sini
  dinamis terhadap `libc`/`libgcc_s`/`libm`, jadi image glibc adalah syarat.
  Image musl (`alpine`) akan gagal dan itu bukan temuan produk.
- `--network none` berarti drill tidak menyentuh jaringan sama sekali. Kalau
  kelak store butuh sinkronisasi keluar, drill ini harus diperluas dan
  batasannya ditulis ulang.
- Nilai cookie tidak pernah dicetak, termasuk di log drill: `cookies ls` sendiri
  tidak mencetaknya, dan skrip tidak mem-parse nilainya.
