---
document_type: project-decision-log
project: "OpenCrabs Browser — mesin buka-web"
status: active
created: "2026-11-04"
---

# OpenCrabs Browser Decision Log

Sumber kebenaran keputusan produk/arsitektur untuk target ini. Kebijakan toolkit
PRD tidak dicatat di sini.

## Decisions

- [2026-11-04 07:0x] Decision: tujuan utama = mata dan tangan di browser
  Status: Accepted
  Approval: owner (verbatim: "Elu punya tangan, mata gitu loh dalam semua hal yang di browser ini elu bisa lihat dan klik semua")
  Requirement IDs: FR-008, FR-009, FR-010
  Context: kendala yang paling sering muncul bukan "browser tidak jalan", tapi gue
    tidak bisa melihat dan mengklik elemen tertentu (custom element, shadow DOM,
    iframe, tombol yang hanya bisa diklik di koordinat).
  Rationale: tanpa persepsi dan aksi yang lengkap, klasifikasi kegagalan cuma
    memberi tahu kenapa gue gagal, bukan membuat gue bisa.
  Impact: milestone 1 berubah dari "diagnostik" menjadi "mata dan tangan";
    diagnostik/escalasi turun ke milestone 2.

- [2026-11-02 04:33] Decision: `max_tabs` ditegakkan di lapisan protokol
  Status: Accepted
  Approval: owner ("Tegakkan max_tabs")
  Requirement IDs: FR-005 (batas), NFR-001 (pagar memori)
  Context: config sudah menyebut `max_tabs` tapi hanya dibaca lalu diabaikan, jadi
    operator mengira ada batas padahal tidak ada.
  Rationale: angka kebenaran dibaca dari mesin (`/json/list`), bukan penghitung
    daemon; pesan penolakan menyebut angka dan cara keluarnya.
  Impact: `/json/new` menjawab 429, `Target.createTarget` menjawab `-32010`, dan
    perintah yang ditolak tidak pernah sampai ke mesin.

- [2026-11-04 06:31] Decision: engine render tetap Chromium, engine ringan hanya jalur teks
  Status: Accepted
  Approval: bukti terukur (bukan preferensi)
  Requirement IDs: FR-001, FR-002
  Context: Lightpanda hemat memori tapi nol mesin layout; Firefox headless tidak
    punya `/json/version` dan idle 896 MB.
  Rationale: screenshot tanpa gaya dan `getBoundingClientRect()` yang melempar
    membuat mesin itu tidak bisa dipakai untuk kerja visual.
  Impact: kandidat pengganti engine keluar dari scope; penghematan diambil dari
    lifecycle (reap, anggaran, batas tab), bukan dari mengganti mesin.

- [2026-11-04 06:31] Decision: `max_tabs` dan `idle_reap_secs` harus berlaku lewat SIGHUP
  Status: Proposed
  Approval: belum ada (menunggu owner)
  Requirement IDs: FR-005, AC-010, AC-011
  Context: hari ini kedua angka dibaca sekali saat start, jadi mengubahnya berarti
    restart daemon dan memutus sesi yang sedang jalan.
  Rationale: reload dengan parse-dulu-baru-tukar menghindari daemon mati karena
    config setengah jadi.
  Impact: `EnginePool` perlu batas yang bisa diperbarui saat hidup; unit systemd
    perlu `ExecReload`.

- [2026-11-04 06:31] Decision: sesi tab memakai lease, bukan satu tab dipakai ulang
  Status: Proposed
  Approval: belum ada (menunggu owner)
  Requirement IDs: FR-005, FR-006
  Context: manajer agent memakai ulang satu tab, jadi kerja paralel saling
    menimpa dan `max_tabs` tidak pernah terasa.
  Rationale: lease memberi batas yang bisa diaudit dan membuat reap tahu tab mana
    yang masih dipakai.
  Impact: butuh state kedua (lease per sesi) yang harus ber-TTL supaya tidak
    membocorkan memori.

## AI Agent Rules

Keputusan berstatus Proposed tidak boleh dianggap izin membangun. Yang Accepted
boleh dirujuk sebagai dasar, dan yang Superseded disimpan, bukan dihapus.