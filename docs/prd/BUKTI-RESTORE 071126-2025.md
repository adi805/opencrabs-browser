# BUKTI: Drill Restore Store `browser-daemon`

Tanggal drill: **2026-11-07 20:24 WIB**. Host: laptop `adijr` (Linux Mint 22.3).

Dokumen ini bukti mentah untuk field **Last verified restore** di
`PRD-MESIN-RUST 071126-1413.md` bagian `Backup & Restore Drill Contract`.
Tanpa berkas ini, field itu cuma tanggal tanpa jejak.

## 1. Apa yang diuji

Unit backup-nya bukan "semua yang penting", tapi dua berkas yang berpasangan:

| Berkas | Ukuran | Peran |
|---|---|---|
| `~/.opencrabs/browser-daemon/store.db` | 53.248 byte | SQLite: tabel `profile`, `session`, `cookie`, `otp_event`, `captcha_event`, `engine_run` |
| `~/.opencrabs/browser-daemon/store.key` | 65 byte (hex) | kunci XChaCha20-Poly1305 32 byte yang mendekripsi nilai cookie |

Kuncinya bagian dari backup, bukan pelengkap: `store.db` yang dipulihkan tanpa
kunci yang cocok tetap terbuka, lalu melaporkan nol cookie, karena
`crates/store/src/crypto.rs` menolak kunci hilang atau salah ketimbang
mengembalikan plaintext (`kunci_berbeda_gagal_membuka_cookie_dengan_pesan_jelas`).
Dan berkas kunci tidak pernah ditimpa, jadi kehilangan kunci = seluruh cookie
lama jadi ciphertext mati.

## 2. Perintah drill

```bash
D=/tmp/oc-restore-drill
LIVE=$HOME/.opencrabs/browser-daemon
mkdir -p "$D/backup" "$D/restored"
cp "$LIVE/store.db" "$D/backup/store.db"
cp "$LIVE/store.key" "$D/backup/store.key"
chmod 600 "$D/backup/store.key"
# simulasi host hilang: pulihkan DARI backup ke direktori bersih
cp "$D/backup/store.db" "$D/restored/store.db"
cp "$D/backup/store.key" "$D/restored/store.key"
chmod 600 "$D/restored/store.key"
sha256sum "$D/backup/store.db" "$D/restored/store.db"
target/debug/browserd cookies ls \
  --db "$D/restored/store.db" --key-file "$D/restored/store.key" --profile scratch
```

## 3. Hasil

| Pemeriksaan | Hasil |
|---|---|
| Integritas byte | kedua sha256 identik: `5e4f31eb840c087ee9a71577f8684ae1ff966ba58eb918dd152a8d435355b69a` (`uniq -c` = 2 baris) |
| Hasil restore dibuka biner | `0 cookie di profil scratch`, exit 0 |
| Mode berkas kunci | `0600` di backup dan di hasil restore (guard `crypto.rs` menolak mode lain) |
| Baris per tabel, sumber vs hasil restore | `profile` 0/0, `session` 0/0, `cookie` 0/0, `engine_run` 0/0 |
| Round-trip nilai terenkripsi di level crate | `cargo test -p store --lib`: **43 passed, 0 failed**, termasuk `cookie_bertahan_setelah_store_dibuka_ulang` dan `kunci_berbeda_gagal_membuka_cookie_dengan_pesan_jelas` |

## 4. Batasan yang jujur

Store di host ini **nol baris cookie**. Jadi drill ini membuktikan:

- pasangan `store.db` + `store.key` selamat melewati salin dan pulih,
- hasil pulih benar-benar bisa dibuka oleh biner dengan kunci yang benar,
- schema dan guard kunci bekerja seperti yang diklaim test crate.

Yang **belum** dibuktikan oleh drill ini: pemulihan cookie milik profil nyata,
karena belum ada profil nyata di host ini. Klaim itu baru sah setelah browserd
dipakai menyimpan cookie satu situs, lalu drill yang sama diulang dan jumlah
baris `cookie` dibandingkan (bukan 0 vs 0).

## 5. Reproduksi

Drill ulang kapan saja dengan blok di bagian 2. Yang wajib diperiksa ulang:
sha256 kedua berkas sama, `browserd cookies ls` exit 0, dan angka `cookie` di
sumber sama dengan di hasil restore. Kalau salah satu berbeda, drill-nya gagal,
bukan "hampir berhasil".
