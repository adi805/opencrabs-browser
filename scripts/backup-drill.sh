#!/usr/bin/env bash
# Latihan backup store ke host kedua: kirim, pulihkan di sana, buka di sana.
#
# # Kenapa skrip, bukan satu tanggal di dokumen
#
# Drill 2026-11-07 berjalan di atas store yang **nol baris cookie**, jadi yang
# terbukti cuma paritas byte: berkasnya selamat, isinya tidak pernah diuji.
# Nol cookie yang dibaca ulang tidak bisa dibedakan dari store rusak yang gagal
# dengan sopan. Skrip ini menuntut hitungan cookie **bukan nol** di host tujuan,
# dan menyertakan kontrol negatif: kunci yang salah harus GAGAL membuka.
#
# # Kenapa nilai cookie sintetis
#
# Yang menyeberang host adalah token sesi kalau dipakai cookie asli. Latihan
# transport tidak butuh kredensial nyata, jadi fixture-nya dari
# `cargo run -p store --example seed_drill`. Skrip ini tidak pernah mencetak
# nilai cookie; `cookies ls` sendiri tidak mencetaknya.
#
# # Pemakaian
#
#   bash scripts/backup-drill.sh --host joyboy --src /tmp/ocb-drill/src \
#        --bin /mnt/hdd/OpenCrabs/build/cargo/release/browserd [--profile drill]
#
#   --src   direktori berisi store.db + store.key (hasil seed_drill)
#   --host  host tujuan, harus bisa di-SSH non-interaktif
#   --bin   biner browserd untuk dijalankan DI HOST TUJUAN
#   --keep  jangan hapus direktori sementara di host tujuan
#   --podman <image>  jalankan pemeriksaan DI DALAM container sekali pakai
#                     (network none, hanya direktori drill yang ter-mount).
#                     Dipakai kalau host tujuan tidak boleh dijadikan tempat
#                     uji: yang menyeberang host tetap store-nya, tapi yang
#                     membukanya container bersih, bukan pustaka host.
#
# Exit 0 hanya kalau semua pemeriksaan lolos. Satu gagal = exit 1, dan
# pemeriksaan sesudahnya tetap dicoba supaya laporan menunjukkan gambaran utuh,
# bukan cuma kegagalan pertama.
set -u

HOST=""
SRC=""
BIN=""
PROFILE="drill"
KEEP=0
PODMAN=""

while [ $# -gt 0 ]; do
  case "$1" in
    --host) HOST="${2:-}"; shift 2 ;;
    --src) SRC="${2:-}"; shift 2 ;;
    --bin) BIN="${2:-}"; shift 2 ;;
    --profile) PROFILE="${2:-}"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --podman) PODMAN="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "argumen tidak dikenal: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$HOST" ] || [ -z "$SRC" ] || [ -z "$BIN" ]; then
  echo "wajib: --host, --src, --bin (lihat --help)" >&2
  exit 2
fi

GAGAL=0
lapor() { printf '%s\n' "$*"; }
cek_bool() {
  # cek_bool <nama> <kondisi 0/1>
  if [ "$2" -eq 0 ]; then
    lapor "  LOLOS  $1"
  else
    lapor "  GAGAL  $1"
    GAGAL=1
  fi
}
cek() {
  # cek <nama> <harapan> <dapat>
  if [ "$2" = "$3" ]; then
    lapor "  LOLOS  $1: $3"
  else
    lapor "  GAGAL  $1: harap $2, dapat $3"
    GAGAL=1
  fi
}

SSH_OPTS="-o BatchMode=yes -o ConnectTimeout=10"
remote() { ssh $SSH_OPTS "$HOST" "$@"; }

# Tanda unik per jalan: nama container tidak boleh bentrok dengan drill lain
# yang mungkin berjalan bersamaan di host yang sama.
TANDA="$$-$(date +%s)"

# Jalankan biner drill. Tanpa --podman: langsung di host tujuan. Dengan
# --podman: di dalam container sekali pakai dengan jaringan dimatikan, dan
# hanya direktori drill yang ter-mount. Container dipakai supaya bukti "store
# bisa dibuka di host lain" tidak bergantung pada pustaka host itu sendiri.
biner() {
  if [ -n "$PODMAN" ]; then
    remote "podman run --rm --name 'ocb-drill-$TANDA' --network none -v '$AKAR:/data:Z' '$PODMAN' /data/browserd $*"
  else
    remote "'$AKAR/browserd' $*"
  fi
}

lapor "== drill backup store -> $HOST =="
lapor "sumber = $SRC"
lapor "biner  = $BIN"

for berkas in store.db store.key; do
  if [ ! -f "$SRC/$berkas" ]; then
    lapor "GAGAL  $SRC/$berkas tidak ada"; exit 1
  fi
done
if [ ! -x "$BIN" ]; then
  lapor "GAGAL  $BIN bukan biner yang bisa dijalankan"; exit 1
fi

MODE_KUNCI=$(stat -c '%a' "$SRC/store.key")
cek "mode kunci di sumber" "600" "$MODE_KUNCI"

SHA_DB=$(sha256sum "$SRC/store.db" | cut -d' ' -f1)
SHA_KEY=$(sha256sum "$SRC/store.key" | cut -d' ' -f1)
UKURAN_DB=$(stat -c '%s' "$SRC/store.db")
lapor "  info   store.db $UKURAN_DB byte, sha256 ${SHA_DB:0:12}"
# Panjang kunci ikut dicatat, isinya tidak pernah.
lapor "  info   store.key $(stat -c '%s' "$SRC/store.key") byte, sha256 ${SHA_KEY:0:12}"

AKAR=$(remote 'mktemp -d /tmp/ocb-backup-drill-XXXXXX') || {
  lapor "GAGAL  tidak bisa membuat direktori di $HOST"; exit 1
}
lapor "tujuan = $HOST:$AKAR"

BERSIHKAN() {
  if [ $KEEP -eq 0 ] && [ -n "$AKAR" ]; then
    remote "rm -rf '$AKAR'" >/dev/null 2>&1 || true
  fi
}
trap BERSIHKAN EXIT

# --- 1. kirim: ini menyeberang host, bukan salin lokal.
remote "mkdir -p '$AKAR/backup' '$AKAR/restored'" || { lapor "GAGAL  mkdir di tujuan"; exit 1; }
scp -q $SSH_OPTS "$SRC/store.db" "$HOST:$AKAR/backup/store.db" || { lapor "GAGAL  kirim store.db"; exit 1; }
scp -q $SSH_OPTS "$SRC/store.key" "$HOST:$AKAR/backup/store.key" || { lapor "GAGAL  kirim store.key"; exit 1; }
scp -q $SSH_OPTS "$BIN" "$HOST:$AKAR/browserd" || { lapor "GAGAL  kirim biner"; exit 1; }
remote "chmod 600 '$AKAR/backup/store.key'; chmod +x '$AKAR/browserd'"
lapor "1. terkirim"

# --- 2. pulihkan ke direktori BERSIH, bukan dibuka langsung dari tempat kiriman.
# Drill yang membuka berkas di direktori asalnya tidak membuktikan langkah pulih.
remote "cp '$AKAR/backup/store.db' '$AKAR/restored/store.db' && cp '$AKAR/backup/store.key' '$AKAR/restored/store.key' && chmod 600 '$AKAR/restored/store.key'"
lapor "2. dipulihkan ke direktori bersih"

# --- 3. paritas byte di tujuan, untuk berkas kiriman DAN hasil pulih.
BACA=$(remote "sha256sum '$AKAR/backup/store.db' '$AKAR/restored/store.db' '$AKAR/restored/store.key'" 2>/dev/null)
SHA_KIRIM=$(printf '%s\n' "$BACA" | awk '/backup\/store.db$/ {print $1}')
SHA_PULIH=$(printf '%s\n' "$BACA" | awk '/restored\/store.db$/ {print $1}')
SHA_KUNCI_PULIH=$(printf '%s\n' "$BACA" | awk '/restored\/store.key$/ {print $1}')
cek "sha256 store.db terkirim" "$SHA_DB" "$SHA_KIRIM"
cek "sha256 store.db hasil pulih" "$SHA_DB" "$SHA_PULIH"
cek "sha256 store.key hasil pulih" "$SHA_KEY" "$SHA_KUNCI_PULIH"
lapor "3. paritas byte diperiksa"

MODE_PULIH=$(remote "stat -c '%a' '$AKAR/restored/store.key'")
cek "mode kunci hasil pulih" "600" "$MODE_PULIH"

# --- 4. buka hasil pulih DENGAN BINER, DI HOST TUJUAN. Ini kriteria utamanya.
# Kalau --podman dipakai, $AKAR ter-mount sebagai /data, jadi path di dalam
# perintah harus path container, bukan path host.
if [ -n "$PODMAN" ]; then
  DIR_DB="/data/restored"; DIR_AKAR="/data"
else
  DIR_DB="$AKAR/restored"; DIR_AKAR="$AKAR"
fi
LS=$(biner "cookies ls --db '$DIR_DB/store.db' --key-file '$DIR_DB/store.key' --profile '$PROFILE'" 2>&1)
RC_LS=$?
JUMLAH=$(printf '%s\n' "$LS" | sed -n 's/^\([0-9][0-9]*\) cookie di profil .*/\1/p' | head -1)
lapor "4. cookies ls di $HOST: ${JUMLAH:-<tidak terbaca>} cookie (exit $RC_LS)"
cek "exit cookies ls" "0" "$RC_LS"
if [ -n "${JUMLAH:-}" ] && [ "$JUMLAH" -gt 0 ]; then
  cek_bool "cookie terbaca di host tujuan bukan nol ($JUMLAH)" 0
else
  cek_bool "cookie terbaca di host tujuan bukan nol (dapat: ${JUMLAH:-<kosong>})" 1
fi

# --- 5. kontrol negatif: kunci yang salah WAJIB gagal membuka.
# Tanpa langkah ini, hitungan bukan nol di atas tidak membedakan "kunci benar"
# dari "store tidak pernah mengenkripsi apa pun".
biner "keygen --key-file '$DIR_AKAR/salah.key' --db '$DIR_AKAR/salah.db'" >/dev/null 2>&1
remote "chmod 600 '$AKAR/salah.key'" >/dev/null 2>&1 || true
SALAH=$(biner "cookies ls --db '$DIR_DB/store.db' --key-file '$DIR_AKAR/salah.key' --profile '$PROFILE'" 2>&1)
RC_SALAH=$?
if [ "$RC_SALAH" -ne 0 ]; then
  lapor "5. kontrol negatif: kunci salah ditolak (exit $RC_SALAH)"
else
  lapor "  GAGAL  kontrol negatif: kunci salah DITERIMA, hitungan di atas tidak membuktikan apa pun"
  GAGAL=1
fi

lapor ""
if [ $GAGAL -eq 0 ]; then
  lapor "LOLOS: $JUMLAH cookie menyeberang host, dipulihkan, dan terbaca kembali di $HOST"
else
  lapor "TIDAK LOLOS: ada pemeriksaan yang gagal di atas"
fi
exit $GAGAL
