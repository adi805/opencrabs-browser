# STATE: Unit 15 & 16 (2026-11-14 17:00 WIB)

## Unit 15 — SELESAI, bukti lengkap

Drill backup store lintas host, commit:
- `942fa80` feat(scripts): drill the store backup across hosts in a throwaway container
- `292e788` docs(prd): record the host-2 backup drill with a non-zero cookie count
- `6fec064` docs(prd): close the off-host backup destination row

Hasil: 5 cookie sintetis menyeberang ke joyboy, dipulihkan di direktori bersih,
dibuka `browserd cookies ls` di dalam container `ubuntu:24.04` (`--network none`),
**5 cookie terbaca**, exit 0. sha256 `47f8c3f3cf95…cc7b8` identik dua sisi, kunci
`0600` dua sisi, kontrol negatif (kunci salah) exit 1. Sesudah drill: nol container
`ocb`, nol `/tmp/ocb-*` di joyboy.

Bukti: `docs/prd/BUKTI-DRILL-HOST2 141126-1650.md`

## Unit 16 — TERBLOKIR pada satu aksi owner

Yang sudah diverifikasi:
- Repo `adi805/opencrabs-web` ADA: private, 83 KB, branch cuma `main`, HEAD `2f227149ddffc71d2c2eac59e7130c353636d98b`
- Backup lokal `~/oc-archive/opencrabs-web.git` HEAD `2f22714` = SHA yang sama, 6 commit, fsck exit 0
- Clone dari backup BERHASIL ke `/mnt/hdd/OpenCrabs/projects/opencrabs-web-restore-test` (636K, isi utuh: index.html, src/, public/, package.json, README.md)
- **Jadi penghapusan remote tidak menghilangkan apa pun**

Yang menghalangi:
- `gh auth status` → token scopes: `gist, read:org, repo, workflow`. **`delete_repo` TIDAK ADA.**
- `gh auth refresh -s delete_repo` butuh browser + device code = interaktif, tidak bisa dari sesi ini.
- Blast radius: menjalankannya non-TTY berisiko merusak token yang dipakai sesi lain. Tidak gue jalankan.

Aksi owner (pilih satu):
1. `gh auth refresh -s delete_repo` di terminal sendiri, lalu `gh repo delete adi805/opencrabs-web --yes`
2. Hapus lewat web UI: Settings → Danger Zone → Delete this repository

Sesudah itu `gh repo view adi805/opencrabs-web` harus exit non-zero = kriteria unit 16.
