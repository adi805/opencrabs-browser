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

## Unit 16: SELESAI 2026-11-14

Repo `adi805/opencrabs-web` sudah dihapus permanen. Tidak ada yang hilang, karena dua backup independen diverifikasi lebih dulu.

Yang diverifikasi SEBELUM hapus:

| Cek | Hasil |
|---|---|
| Bare clone `~/oc-archive/opencrabs-web.git` | 212K, `fsck` tanpa keluaran (bersih), `refs/heads/main` = `2f227149ddffc71d2c2eac59e7130c353636d98b`, 6 commit, 0 tag |
| Remote HEAD saat itu | `2f227149ddffc71d2c2eac59e7130c353636d98b`, SHA sama persis |
| Bundle `~/oc-archive/opencrabs-web-141126-1707.bundle` | 80K, `bundle verify` = "records a complete history", sha256 `570f678f360acf2fd2c50f45f3ac6ef758b0dd27c2cc457e1277daa8b76de1ab` |
| Clone dari backup | pernah BERHASIL ke `/mnt/hdd/OpenCrabs/projects/opencrabs-web-restore-test` (636K, isi utuh: index.html, src/, public/, package.json, README.md) |

Bukti hapus:

| Perintah | Keluaran |
|---|---|
| `gh repo delete adi805/opencrabs-web --yes` | RC=0 |
| `gh repo view adi805/opencrabs-web` | `GraphQL: Could not resolve to a Repository with the name 'adi805/opencrabs-web'.` RC=1 |
| `gh api repos/adi805/opencrabs-web` | HTTP 404 `Not Found` |

Kriteria unit 16 (`gh repo view adi805/opencrabs-web` exit non-zero) terpenuhi. Token sekarang membawa scope `delete_repo`, jadi aksi owner yang dulu menghalangi sudah dilakukan.
