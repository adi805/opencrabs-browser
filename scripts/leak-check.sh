#!/usr/bin/env bash
# Gagal (exit 1) kalau masih ada mesin browser atau daemon uji yang hidup.
#
# # Kenapa penanda, bukan nama biner
#
# Host ini juga menjalankan browser produksi (port 9222) dan daemon
# pengembangan (9224/9225). Menghitung semua proses `chrome` atau semua
# `browserd` berarti menuduh mesin milik pemilik host, dan gate yang menuduh
# orang lain akan dimatikan orang. Setiap mesin dan daemon uji membuat profil
# di bawah `/tmp/ocb-*`, jadi penanda itulah yang memisahkannya.
#
# # Kenapa daemon ikut diperiksa
#
# Versi sebelumnya hanya mencocokkan `chrome`. `browserd serve` yang tertinggal
# dari uji native lolos diam-diam: dia memegang port dan RAM, tapi tidak
# pernah disebut bocor karena namanya bukan chrome.
#
# # Kenapa penanda dibangun saat runtime
#
# Kalau penanda ditulis utuh di baris perintah, proses pemanggilnya ikut cocok
# dan skrip menuduh dirinya sendiri. Karena itu penanda disusun dari potongan.
#
# # Pemakaian
#
#   bash scripts/leak-check.sh             periksa host, exit 0 bersih / 1 bocor
#   bash scripts/leak-check.sh --self-test buktikan pemeriksanya bisa gagal
#
# `--self-test` menanam satu proses yang membawa penanda, memastikan pemeriksa
# menemukannya, lalu mematikannya. Tanpa itu, penanda yang salah ketik membuat
# pemeriksa selalu hijau dan gate-nya cuma hiasan.
set -u

python3 - "$@" << 'PYEOF'
import os
import subprocess
import sys
import time

# Disusun dari potongan supaya baris perintah pemanggil tidak ikut cocok.
PENANDA = "/tmp/" + "ocb-"
MESIN = "chrome"
DAEMON = "browser" + "d"
# Proses yang selalu ada di host ini dan bukan milik uji. Dicocokkan sebagai
# potongan path lengkap, bukan nama pendek, supaya tidak ada mesin uji yang
# lolos hanya karena kebetulan punya potongan yang sama.
ABAIKAN = ()


def cmdline(pid):
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            return f.read().decode("utf-8", "replace").replace("\x00", " ").strip()
    except OSError:
        return ""


def pindai():
    bocor = []
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        isi = cmdline(pid)
        if not isi or PENANDA not in isi:
            continue
        if any(potongan in isi for potongan in ABAIKAN):
            continue
        jenis = None
        if MESIN in isi:
            jenis = "mesin"
        elif DAEMON in isi:
            jenis = "daemon"
        if jenis:
            bocor.append((int(pid), jenis, isi))
    return sorted(bocor)


def laporkan(bocor):
    if not bocor:
        print("BERSIH: nol mesin dan nol daemon uji tersisa")
        return 0
    print(f"BOCOR: {len(bocor)} proses uji masih hidup")
    for pid, jenis, isi in bocor:
        print(f"  [{jenis}] {pid} {isi[:140]}")
    return 1


def self_test():
    # Proses yang membawa penanda sekaligus nama daemon. `sleep 30; :` menjaga
    # bash tetap hidup sebagai induk: dengan satu perintah saja bash mengganti
    # dirinya lewat exec dan argv yang memuat penanda ikut hilang.
    perintah = f"sleep 30; : # {DAEMON} --config {PENANDA}planted/browserd.toml"
    anak = subprocess.Popen(["bash", "-c", perintah], stdin=subprocess.DEVNULL)

    ketemu = []
    for _ in range(50):
        ketemu = [b for b in pindai() if b[0] == anak.pid]
        if ketemu:
            break
        time.sleep(0.02)

    anak.kill()
    anak.wait()

    if ketemu:
        print(f"SELF-TEST LOLOS: pemeriksa menemukan proses {anak.pid} yang ditanam")
        return 0
    print(f"SELF-TEST GAGAL: pemeriksa tidak menemukan proses {anak.pid} yang ditanam")
    return 1


if "--self-test" in sys.argv[1:]:
    sys.exit(self_test())
sys.exit(laporkan(pindai()))
PYEOF
