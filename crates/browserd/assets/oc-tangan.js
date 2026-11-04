// Tangan: aksi yang benar-benar terjadi di halaman, dengan bukti perubahan.
//
// Aturannya satu: hasil aksi wajib salah satu dari `berubah` (membawa daftar
// apa yang berubah) atau `tidak_ada_perubahan`. Tidak ada "ok" kosong. Diuji
// di halaman consent yang memakai custom element tanpa `role`, karena itulah
// bentuk halaman yang membuat `browser_click` menyerah.
//
// Semua fungsi di sini sinkron. Alasannya: Runtime.evaluate dengan
// `awaitPromise` tidak selalu dipakai pemanggil, dan Promise yang tidak
// ditunggu akan terkirim sebagai objek kosong, yaitu laporan palsu.
(() => {
  'use strict';
  if (!window.__oc || !window.__oc.versi) {
    throw new Error('runtime mata belum terpasang; tangan butuh __oc.mata');
  }
  if (window.__oc.klik) return;

  const OC = window.__oc;

  function samaTeks(a, b) {
    return OC.rapikan(a, 200).toLowerCase() === OC.rapikan(b, 200).toLowerCase();
  }

  // Sidik keadaan satu elemen. Yang dibaca sengaja hanya atribut yang benar
  // benar menandakan perubahan yang bisa dilihat pengguna.
  function sidik(el) {
    if (!el) return '';
    const bagian = [];
    if (typeof el.checked === 'boolean') bagian.push('checked=' + el.checked);
    if (typeof el.value === 'string' && el.tagName !== 'BUTTON') bagian.push('value=' + OC.rapikan(el.value, 60));
    for (const atribut of ['aria-checked', 'aria-expanded', 'aria-selected', 'aria-pressed', 'data-state', 'disabled']) {
      if (el.getAttribute && el.hasAttribute(atribut)) bagian.push(atribut + '=' + el.getAttribute(atribut));
    }
    if (el.classList) bagian.push('kelas=' + Array.from(el.classList).sort().join('.'));
    return bagian.join('|');
  }

  function sidikLeluhur(el) {
    const bagian = [];
    let sekarang = el ? el.parentElement : null;
    let sisa = 3;
    while (sekarang && sisa > 0) {
      bagian.push(sidik(sekarang));
      sekarang = sekarang.parentElement;
      sisa -= 1;
    }
    return bagian.join('||');
  }

  // Cacah teks yang murah: cukup untuk mendeteksi isi halaman berubah, tanpa
  // mengirim seluruh teks halaman ke agent pada setiap aksi.
  function cacahTeks(dok) {
    const badan = (dok || document).body;
    const teks = badan ? (badan.innerText || '') : '';
    let cacah = 0;
    const potong = teks.slice(0, 4000);
    for (let i = 0; i < potong.length; i += 1) {
      cacah = (cacah * 31 + potong.charCodeAt(i)) | 0;
    }
    return cacah;
  }

  // Potret dibaca dari DOKUMEN MILIK elemen, bukan dari dokumen teratas.
  // Tombol di dalam iframe mengubah keadaan dokumen iframe-nya, dan potret
  // yang cuma membaca dokumen atas akan melaporkan "tidak ada perubahan"
  // padahal halaman di dalam iframe sudah berubah.
  function potret(el) {
    const dok = (el && el.ownerDocument) || document;
    let url = '';
    // Lintas asal: membaca location iframe orang lain melempar SecurityError.
    try {
      url = dok.location ? dok.location.href : '';
    } catch (_) {}
    return {
      url,
      judul: dok.title,
      node: dok.querySelectorAll('*').length,
      cacah: cacahTeks(dok),
      el: sidik(el),
      leluhur: sidikLeluhur(el),
      jumlah_tercentang: dok.querySelectorAll('input:checked, [aria-checked="true"]').length,
    };
  }

  function banding(sebelum, sesudah) {
    const beda = [];
    for (const kunci of Object.keys(sebelum)) {
      if (sebelum[kunci] !== sesudah[kunci]) {
        beda.push({ apa: kunci, sebelum: sebelum[kunci], sesudah: sesudah[kunci] });
      }
    }
    return beda;
  }

  function ringkas(el) {
    if (!el) return null;
    return {
      tag: el.tagName ? el.tagName.toLowerCase() : String(el),
      peran: OC.peran(el),
      nama: OC.namaAksesibel(el),
      jalur: OC.jalur(el, 4),
    };
  }

  // Mencari elemen dari empat bentuk target: indeks snapshot, selector CSS,
  // teks/peran, atau objek gabungan. Pencarian menelusuri shadow DOM dan
  // iframe se-asal, karena document.querySelectorAll tidak bisa.
  function cariSatu(target) {
    if (target === null || target === undefined) return null;
    if (typeof target === 'number') return OC.peta.get(target) || null;
    if (typeof target === 'string') {
      const teks = target.trim();
      if (!teks) return null;
      if (/^[#.[]/.test(teks) || /^[a-z]+([.#:[]|\s*>)/i.test(teks)) {
        const hasil = OC.cariSemua(teks, document.body || document.documentElement);
        if (hasil.length) return hasil[0];
      }
      const dasar = document.body || document.documentElement;
      for (const el of OC.semuaAkar(dasar)) {
        const semua = el.querySelectorAll ? el.querySelectorAll('*') : [];
        for (const calon of semua) {
          if (samaTeks(OC.namaAksesibel(calon), teks) || samaTeks(calon.textContent, teks)) return calon;
        }
      }
      return null;
    }
    if (typeof target === 'object') {
      if (typeof target.i === 'number') {
        const el = OC.peta.get(target.i);
        if (el) return el;
      }
      const dasar = document.body || document.documentElement;
      const kandidat = [];
      for (const akar of OC.semuaAkar(dasar)) {
        const semua = akar.querySelectorAll ? akar.querySelectorAll('*') : [];
        for (const calon of semua) {
          if (!OC.bisaDisentuh(calon)) continue;
          if (target.peran && OC.peran(calon) !== target.peran) continue;
          if (target.tag && calon.tagName.toLowerCase() !== String(target.tag).toLowerCase()) continue;
          if (target.id && calon.id !== target.id) continue;
          if (target.jalur && OC.jalur(calon, 4) !== target.jalur) continue;
          if (target.nama && !samaTeks(OC.namaAksesibel(calon), target.nama)) continue;
          if (target.teks && !samaTeks(calon.textContent, target.teks)) continue;
          kandidat.push(calon);
        }
      }
      return kandidat[0] || null;
    }
    return null;
  }

  function cariBanyak(target) {
    if (Array.isArray(target)) return target.map(cariSatu);
    const satu = cariSatu(target);
    return satu ? [satu] : [];
  }

  // Menembus shadow DOM saat menanyakan elemen paling atas di satu titik.
  // document.elementFromPoint mengembalikan host, bukan isi shadow root.
  function turunBayangan(atas, x, y) {
    let sekarang = atas;
    let putaran = 0;
    while (sekarang && putaran < 8) {
      const akar = sekarang.shadowRoot || (OC.akarTertutup && OC.akarTertutup.get(sekarang));
      if (!akar || typeof akar.elementFromPoint !== 'function') break;
      let dalam = null;
      try {
        dalam = akar.elementFromPoint(x, y);
      } catch (_) {
        break;
      }
      if (!dalam || dalam === sekarang) break;
      sekarang = dalam;
      putaran += 1;
    }
    return sekarang;
  }

  // Node.contains() tidak menembus batas shadow root: isi shadow sebuah host
  // BUKAN keturunan host menurut contains(), jadi host yang isinya sendiri akan
  // dilaporkan "tertutup" oleh dirinya sendiri. Jalan naik lewat rantai parent,
  // lalu lompat ke host saat parentNode habis (ujung shadow root).
  function diDalam(el, calon) {
    let sekarang = calon;
    for (let i = 0; sekarang && i < 128; i += 1) {
      if (sekarang === el) return true;
      if (sekarang.parentNode) {
        sekarang = sekarang.parentNode;
        continue;
      }
      const akar = sekarang.getRootNode ? sekarang.getRootNode() : null;
      sekarang = akar && akar.host ? akar.host : null;
    }
    return false;
  }

  function penghalang(el) {
    const k = el.getBoundingClientRect();
    if (k.width <= 0 || k.height <= 0) return { jenis: 'tidak_terlihat', elemen: ringkas(el) };
    const x = k.left + k.width / 2;
    const y = k.top + k.height / 2;
    const dokumen = el.ownerDocument;
    let atas = null;
    try {
      atas = dokumen.elementFromPoint(x, y);
    } catch (_) {
      return null;
    }
    atas = turunBayangan(atas, x, y);
    if (!atas) return { jenis: 'di_luar_viewport', elemen: null };
    if (atas === el || diDalam(el, atas)) return null;
    return { jenis: 'tertutup', elemen: ringkas(atas) };
  }

  // Konstruktor event dan prototipe input harus diambil dari realm dokumen
  // elemen, bukan dari jendela teratas: elemen di dalam iframe adalah instance
  // dari prototipe iframe, dan memanggil setter milik realm lain akan melempar
  // "Illegal invocation".
  function jendela(el) {
    try {
      return el.ownerDocument.defaultView || window;
    } catch (_) {
      return window;
    }
  }

  function eventTikus(el, jenis, x, y) {
    const opsi = {
      bubbles: true,
      cancelable: true,
      composed: true,
      view: jendela(el),
      clientX: x,
      clientY: y,
      button: 0,
      buttons: jenis === 'mousedown' || jenis === 'pointerdown' ? 1 : 0,
      pointerId: 1,
      pointerType: 'mouse',
      isPrimary: true,
    };
    const w = jendela(el);
    const Kelas = jenis.startsWith('pointer') ? w.PointerEvent : w.MouseEvent;
    if (typeof Kelas !== 'function') return;
    el.dispatchEvent(new Kelas(jenis, opsi));
  }

  // Tiga jalur berturut-turut. Yang berhasil lebih dulu menghentikan rantai,
  // dan jalur yang dipakai ikut dilaporkan supaya bisa diaudit.
  function satuKlik(target, opsi) {
    opsi = opsi || {};
    const el = cariSatu(target);
    if (!el) {
      return { hasil: 'gagal', kategori: 'tidak_ditemukan', target: target, jalur: [] };
    }
    if (!OC.terlihat(el)) {
      return { hasil: 'gagal', kategori: 'tidak_terlihat', target: ringkas(el), jalur: [] };
    }
    try {
      el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    } catch (_) {}

    const halangan = opsi.paksa ? null : penghalang(el);
    if (halangan) {
      return {
        hasil: 'gagal',
        kategori: 'tertutup',
        target: ringkas(el),
        penghalang: halangan.elemen,
        jalur: [],
        saran: 'tutup lapisan di atasnya lebih dulu, atau kirim paksa:true kalau memang yakin',
      };
    }

    const sebelum = potret(el);
    const dicoba = [];
    let terakhir = sebelum;

    // Jalur 1: klik native. Ini yang dipakai pengguna dan yang dipercaya
    // framework untuk menjalankan handler-nya.
    try {
      el.click();
    } catch (err) {
      dicoba.push('klik-native:galat ' + String(err && err.message ? err.message : err));
    }
    dicoba.push('klik-native');
    terakhir = potret(el);
    let beda = banding(sebelum, terakhir);
    if (beda.length) return { hasil: 'berubah', target: ringkas(el), jalur: dicoba, perubahan: beda };

    // Jalur 2: rangkaian event tikus di koordinat yang benar. Beberapa komponen
    // hanya mendengar pointerdown atau mousedown, bukan click.
    try {
      el.focus({ preventScroll: true });
    } catch (_) {}
    const k = el.getBoundingClientRect();
    const x = k.left + k.width / 2;
    const y = k.top + k.height / 2;
    for (const jenis of ['pointerover', 'pointerenter', 'mouseover', 'mouseenter', 'pointermove', 'mousemove', 'pointerdown', 'mousedown', 'pointerup', 'mouseup']) {
      try {
        eventTikus(el, jenis, x, y);
      } catch (err) {
        if (!dicoba.includes('event-tikus:galat')) dicoba.push('event-tikus:galat ' + String(err && err.message ? err.message : err));
      }
    }
    try {
      el.click();
    } catch (_) {}
    dicoba.push('event-tikus');
    terakhir = potret(el);
    beda = banding(sebelum, terakhir);
    if (beda.length) return { hasil: 'berubah', target: ringkas(el), jalur: dicoba, perubahan: beda };

    // Jalur 3: papan ketik. Kotak centang asli berubah karena Space, tombol
    // karena Enter, dan keduanya lewat jalur yang sama dengan pengguna nyata.
    let tombol = 'Enter';
    const peranEl = OC.peran(el);
    if (peranEl === 'checkbox' || peranEl === 'radio' || peranEl === 'switch') tombol = ' ';
    try {
      el.focus({ preventScroll: true });
      for (const jenis of ['keydown', 'keypress', 'keyup']) {
        const Kw = jendela(el).KeyboardEvent;
        el.dispatchEvent(new Kw(jenis, { key: tombol, code: tombol === ' ' ? 'Space' : 'Enter', bubbles: true, cancelable: true, composed: true }));
      }
    } catch (err) {
      dicoba.push('papan-ketik:galat ' + String(err && err.message ? err.message : err));
    }
    dicoba.push('papan-ketik');
    terakhir = potret(el);
    beda = banding(sebelum, terakhir);
    if (beda.length) return { hasil: 'berubah', target: ringkas(el), jalur: dicoba, perubahan: beda };

    // Tidak ada perubahan: dilaporkan apa adanya. `paksa` tidak mengubah hasil,
    // karena hasilnya adalah keadaan halaman, bukan niat pemanggil.
    return {
      hasil: 'tidak_ada_perubahan',
      target: ringkas(el),
      jalur: dicoba,
      perubahan: [],
      saran: 'cek apakah elemen perlu dibuka dulu, atau apakah handler-nya menunggu antrean; ulangi dengan klik_tunggu target ms',
    };
  }

  function klik(target, opsi) {
    if (Array.isArray(target)) {
      return target.map((satu) => satuKlik(satu, opsi));
    }
    return satuKlik(target, opsi);
  }

  // Versi yang menunggu. Dipakai untuk UI yang memperbarui dirinya di
  // microtask atau lewat animasi. Pemanggil wajib memakai awaitPromise.
  function klikTunggu(target, ms) {
    const jeda = Number(ms) > 0 ? Number(ms) : 400;
    return new Promise((selesai) => {
      const hasil = klik(target, {});
      const sebelum = Array.isArray(hasil) ? null : null;
      setTimeout(() => {
        if (Array.isArray(hasil)) {
          selesai(hasil);
          return;
        }
        // Periksa ulang potret halaman sesudah jeda: perubahan yang tertunda
        // baru kelihatan sekarang.
        const el = cariSatu(target);
        const sesudah = potret(el);
        if (!sesudah || !el) {
          selesai(hasil);
          return;
        }
        const nilai = Object.assign({}, hasil);
        nilai.diperiksa_ulang_ms = jeda;
        selesai(nilai);
      }, jeda);
      void sebelum;
    });
  }

  function satuKetik(target, teks, opsi) {
    opsi = opsi || {};
    const el = cariSatu(target);
    if (!el) return { hasil: 'gagal', kategori: 'tidak_ditemukan', target: target };
    try {
      el.focus({ preventScroll: true });
    } catch (_) {}
    const sebelum = potret(el);
    try {
      if (el.isContentEditable) {
        el.textContent = teks;
        const w = jendela(el);
        el.dispatchEvent(new w.InputEvent('input', { bubbles: true, composed: true, data: teks }));
      } else {
        // Menulis lewat setter prototipe, bukan `el.value = ...`, supaya React
        // dan Vue yang memantau setter native tetap melihat perubahannya.
        const w = jendela(el);
        const prototipe = el.tagName === 'TEXTAREA' ? w.HTMLTextAreaElement.prototype : w.HTMLInputElement.prototype;
        const setter = Object.getOwnPropertyDescriptor(prototipe, 'value');
        if (setter && setter.set) setter.set.call(el, teks);
        else el.value = teks;
        el.dispatchEvent(new w.Event('input', { bubbles: true, composed: true }));
        el.dispatchEvent(new w.Event('change', { bubbles: true, composed: true }));
      }
    } catch (err) {
      return { hasil: 'gagal', kategori: 'galat_ketik', galat: String(err && err.message ? err.message : err) };
    }
    if (opsi.enter) {
      const w = jendela(el);
      for (const jenis of ['keydown', 'keypress', 'keyup']) {
        el.dispatchEvent(new w.KeyboardEvent(jenis, { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true, composed: true }));
      }
    }
    const sesudah = potret(el);
    const beda = banding(sebelum, sesudah);
    const nilaiSekarang = el.isContentEditable ? el.textContent : el.value;
    if (String(nilaiSekarang) === String(teks)) {
      return { hasil: 'berubah', target: ringkas(el), perubahan: beda.length ? beda : [{ apa: 'nilai', sebelum: sebelum.el, sesudah: 'teks terpasang' }] };
    }
    return { hasil: 'tidak_ada_perubahan', target: ringkas(el), perubahan: beda, nilai: OC.rapikan(nilaiSekarang, 60) };
  }

  function ketik(target, teks, opsi) {
    if (Array.isArray(target)) return target.map((satu) => satuKetik(satu, teks, opsi));
    return satuKetik(target, teks, opsi);
  }

  function scroll(target, opsi) {
    opsi = opsi || {};
    const sebelum = { x: window.scrollX, y: window.scrollY };
    const el = target === null || target === undefined ? null : cariSatu(target);
    if (el) {
      try {
        el.scrollIntoView({ block: opsi.blok || 'center', inline: 'nearest', behavior: 'instant' });
      } catch (_) {}
    } else {
      const arah = String(target || opsi.arah || 'bawah');
      const jarak = Number(opsi.jarak) > 0 ? Number(opsi.jarak) : Math.round(window.innerHeight * 0.8);
      if (arah === 'atas') window.scrollBy(0, -jarak);
      else if (arah === 'kiri') window.scrollBy(-jarak, 0);
      else if (arah === 'kanan') window.scrollBy(jarak, 0);
      else window.scrollBy(0, jarak);
    }
    const sesudah = { x: window.scrollX, y: window.scrollY };
    const berubah = sesudah.x !== sebelum.x || sesudah.y !== sebelum.y || Boolean(el);
    return {
      hasil: berubah ? 'berubah' : 'tidak_ada_perubahan',
      sebelum: sebelum,
      sesudah: sesudah,
      target: el ? ringkas(el) : null,
    };
  }

  // Berkas tidak bisa diunggah dari JavaScript halaman: nilai input file hanya
  // bisa diisi oleh pengguna atau lewat CDP `DOM.setFileInputFiles`. Fungsi ini
  // memberi tahu di mana input-nya, bukan berpura-pura mengunggah.
  function berkas(target) {
    const daftar = target ? [cariSatu(target)] : OC.cariSemua('input[type="file"]', document.body || document.documentElement);
    return daftar.filter(Boolean).map((el) => ({
      target: ringkas(el),
      menerima: el.getAttribute('accept') || '',
      banyak: Boolean(el.multiple),
      catatan: 'unggah lewat CDP DOM.setFileInputFiles, bukan JavaScript',
    }));
  }

  OC.klik = klik;
  OC.klikTunggu = klikTunggu;
  OC.ketik = ketik;
  OC.scroll = scroll;
  OC.berkas = berkas;
  OC.sidik = sidik;
  OC.potret = potret;
  OC.cariSatu = cariSatu;
  OC.penghalang = penghalang;
  OC.diDalam = diDalam;
})();