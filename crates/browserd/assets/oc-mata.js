// Mata: persepsi halaman untuk agent.
//
// Kenapa file ini ada: `browser_click` bergantung pada selector yang resmi ada,
// dan halaman nyata (consent Google, panel SPA, komponen web) memakai custom
// element yang tidak punya `role` maupun selector stabil. Tanpa mata, agent
// hanya bisa menebak. Berkas ini memberi dua hal yang bisa dibaca: daftar
// elemen yang benar-benar terlihat dan bisa disentuh, dan isi DOM yang ringkas.
//
// Berkas ini disuntikkan pada document-start, bukan ditempel ke halaman yang
// sudah jadi. Alasannya konkret: shadow root bermode `closed` hanya bisa
// direkam pada detik ia dibuat. Halaman yang sudah selesai dimuat sebelum
// suntikan tidak bisa lagi dilihat isinya.
(() => {
  'use strict';

  // Suntikan ganda tidak boleh menghapus rekaman yang sudah ada: addScript dan
  // Runtime.evaluate sering dipanggil berurutan untuk dokumen yang sama.
  if (window.__oc && window.__oc.versi) return;

  const VERSI = '1';

  // Shadow root `closed` tidak punya jalan masuk dari luar host. Satu-satunya
  // cara membacanya adalah mencatat objeknya saat dipanggil, lalu menyimpannya
  // di WeakMap yang tidak bisa diintip halaman.
  const akarTertutup = new WeakMap();
  const pasangAsli = Element.prototype.attachShadow;
  if (typeof pasangAsli === 'function') {
    Element.prototype.attachShadow = function (init) {
      const akar = pasangAsli.call(this, init);
      try {
        if (init && init.mode === 'closed') akarTertutup.set(this, akar);
      } catch (_) {
        // Host yang membekukan dirinya sendiri tidak boleh membuat halaman rusak.
      }
      return akar;
    };
  }

  const TAG_INTERAKTIF = new Set([
    'BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'SUMMARY', 'OPTION',
  ]);
  const PERAN_INTERAKTIF = new Set([
    'button', 'link', 'checkbox', 'radio', 'textbox', 'combobox', 'tab',
    'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option', 'switch',
    'slider', 'spinbutton', 'searchbox', 'treeitem', 'gridcell',
  ]);
  const TAG_NON_INTERAKTIF = new Set([
    'HTML', 'HEAD', 'BODY', 'SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'META',
    'LINK', 'TITLE', 'BR', 'HR', 'IMG', 'SVG', 'PATH', 'SPAN', 'DIV', 'P',
    'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'UL', 'OL', 'LI', 'TABLE', 'TR', 'TD',
    'TH', 'THEAD', 'TBODY', 'FORM', 'SECTION', 'ARTICLE', 'HEADER', 'FOOTER',
    'MAIN', 'NAV', 'ASIDE', 'LABEL', 'FIELDSET', 'LEGEND', 'IFRAME', 'FRAME',
  ]);

  // Peta indeks ke elemen untuk snapshot terakhir. Aksi memakai indeks ini
  // supaya agent tidak perlu mengirim selector yang rapuh.
  const peta = new Map();
  let nomorTerakhir = 0;

  function rapikan(teks, batas) {
    if (!teks) return '';
    return String(teks).replace(/\s+/g, ' ').trim().slice(0, batas || 120);
  }

  function terlihat(el) {
    if (!el || !el.getBoundingClientRect) return false;
    const kotak = el.getBoundingClientRect();
    if (kotak.width <= 0 || kotak.height <= 0) return false;
    if (typeof el.checkVisibility === 'function') {
      try {
        return el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true });
      } catch (_) {
        // Versi Chromium lama tidak mengenal opsi ini.
      }
    }
    const gaya = gayaDari(el);
    return Boolean(gaya) && gaya.visibility !== 'hidden' && gaya.display !== 'none' && Number(gaya.opacity) > 0.01;
  }

  function gayaDari(el) {
    try {
      return el.ownerDocument.defaultView.getComputedStyle(el);
    } catch (_) {
      return null;
    }
  }

  function peran(el) {
    const eksplisit = el.getAttribute && el.getAttribute('role');
    if (eksplisit) return eksplisit.trim().split(/\s+/)[0];
    const tag = el.tagName;
    if (tag === 'A') return el.hasAttribute('href') ? 'link' : 'generik';
    if (tag === 'BUTTON') return 'button';
    if (tag === 'SELECT') return el.multiple ? 'listbox' : 'combobox';
    if (tag === 'TEXTAREA') return 'textbox';
    if (tag === 'SUMMARY') return 'button';
    if (tag === 'OPTION') return 'option';
    if (tag === 'INPUT') {
      const jenis = (el.getAttribute('type') || 'text').toLowerCase();
      if (jenis === 'checkbox') return 'checkbox';
      if (jenis === 'radio') return 'radio';
      if (jenis === 'range') return 'slider';
      if (jenis === 'number') return 'spinbutton';
      if (jenis === 'search') return 'searchbox';
      if (jenis === 'submit' || jenis === 'button' || jenis === 'reset' || jenis === 'file') return 'button';
      if (jenis === 'hidden') return 'none';
      return 'textbox';
    }
    const tab = el.getAttribute && el.getAttribute('tabindex');
    if (tab !== null && Number(tab) >= 0) return 'generik-fokus';
    if (el.isContentEditable) return 'textbox';
    return 'generik';
  }

  function namaAksesibel(el) {
    const label = (el.getAttribute && el.getAttribute('aria-label')) || '';
    if (label.trim()) return rapikan(label);

    const rujukan = (el.getAttribute && el.getAttribute('aria-labelledby')) || '';
    if (rujukan) {
      const teks = rujukan
        .split(/\s+/)
        .map((id) => {
          const t = el.ownerDocument.getElementById(id);
          return t ? t.textContent : '';
        })
        .join(' ');
      if (teks.trim()) return rapikan(teks);
    }

    if (el.tagName === 'IMG') return rapikan(el.getAttribute('alt'));

    if (el.id) {
      try {
        const pasangan = el.ownerDocument.querySelector('label[for="' + CSS.escape(el.id) + '"]');
        if (pasangan) return rapikan(pasangan.textContent);
      } catch (_) {}
    }
    const indukLabel = el.closest ? el.closest('label') : null;
    if (indukLabel && indukLabel !== el) return rapikan(indukLabel.textContent);

    const judul = el.getAttribute && el.getAttribute('title');
    if (judul && judul.trim()) return rapikan(judul);

    if (el.tagName === 'INPUT') {
      const jenis = (el.getAttribute('type') || '').toLowerCase();
      if (jenis === 'submit' || jenis === 'button') return rapikan(el.value);
      if (el.value && jenis !== 'password') return rapikan(el.value);
    }
    if (el.placeholder) return rapikan(el.placeholder);
    return rapikan(el.textContent, 80);
  }

  function bisaDisentuh(el) {
    if (TAG_INTERAKTIF.has(el.tagName)) {
      const jenis = (el.getAttribute('type') || '').toLowerCase();
      if (el.tagName === 'INPUT' && jenis === 'hidden') return false;
      return true;
    }
    if (PERAN_INTERAKTIF.has(peran(el))) return true;
    if (el.isContentEditable) return true;
    const tab = el.getAttribute && el.getAttribute('tabindex');
    if (tab !== null && Number(tab) >= 0) return true;
    for (const nama of ['onclick', 'onmousedown', 'onpointerdown']) {
      if (el.hasAttribute && el.hasAttribute(nama)) return true;
    }
    if (typeof el.onclick === 'function') return true;
    const gaya = gayaDari(el);
    return Boolean(gaya) && gaya.cursor === 'pointer';
  }

  function jalur(el, batas) {
    const bagian = [];
    let sekarang = el;
    let sisa = batas || 4;
    while (sekarang && sekarang.nodeType === 1 && sisa > 0) {
      let potong = sekarang.tagName.toLowerCase();
      if (sekarang.id) {
        bagian.unshift(potong + '#' + sekarang.id);
        break;
      }
      const kelas = (sekarang.getAttribute('class') || '').trim().split(/\s+/).filter(Boolean)[0];
      if (kelas) potong += '.' + kelas.replace(/[^\w-]/g, '');
      bagian.unshift(potong);
      sekarang = sekarang.parentElement;
      sisa -= 1;
    }
    return bagian.join(' > ');
  }

  function kotakDari(el) {
    const k = el.getBoundingClientRect();
    return {
      x: Math.round(k.left),
      y: Math.round(k.top),
      lebar: Math.round(k.width),
      tinggi: Math.round(k.height),
    };
  }

  function rekam(el, asal) {
    const catatan = {
      i: ++nomorTerakhir,
      tag: el.tagName.toLowerCase(),
      peran: peran(el),
      nama: namaAksesibel(el),
      teks: rapikan(el.textContent, 100),
      kotak: kotakDari(el),
      terlihat: terlihat(el),
      asal: asal,
      jalur: jalur(el),
    };
    if ('checked' in el && typeof el.checked === 'boolean') catatan.tercentang = el.checked;
    if ('value' in el && typeof el.value === 'string') catatan.nilai = rapikan(el.value, 80);
    for (const atribut of ['aria-checked', 'aria-expanded', 'aria-selected', 'aria-pressed', 'aria-disabled', 'aria-hidden']) {
      if (el.hasAttribute && el.hasAttribute(atribut)) catatan[atribut] = el.getAttribute(atribut);
    }
    if (el.id) catatan.id = el.id;
    peta.set(catatan.i, el);
    return catatan;
  }

  // Menelusuri seluruh akar: DOM utama, shadow root terbuka, shadow root
  // tertutup yang terekam, dan dokumen iframe se-asal. Iframe lintas asal tidak
  // bisa dibaca dari halaman ini, dan itu dicatat sebagai wadah, bukan disembunyikan.
  function jelajah(akar, asal, wadah, hasil, batas) {
    if (!akar || hasil.length >= batas) return;
    const elemen = akar.querySelectorAll ? akar.querySelectorAll('*') : [];
    for (const el of elemen) {
      if (hasil.length >= batas) return;
      const bayangan = el.shadowRoot || akarTertutup.get(el);
      if (bayangan) {
        wadah.push({ jenis: 'shadow', host: jalur(el, 3), mode: el.shadowRoot ? 'open' : 'closed' });
        jelajah(bayangan, asal === 'dom' ? 'shadow' : asal, wadah, hasil, batas);
      }
      if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {
        let dokumen = null;
        try {
          dokumen = el.contentDocument;
        } catch (_) {
          dokumen = null;
        }
        if (dokumen && dokumen.documentElement && dokumen.body) {
          wadah.push({ jenis: 'iframe', host: jalur(el, 3), src: el.getAttribute('src') || '' });
          jelajah(dokumen.body, 'iframe', wadah, hasil, batas);
        } else {
          wadah.push({ jenis: 'iframe', host: jalur(el, 3), src: el.getAttribute('src') || '', lintas_asal: true });
        }
      }
      if (!bisaDisentuh(el)) continue;
      hasil.push(rekam(el, asal));
    }
  }

  function ringkasanTeks(batas) {
    const badan = document.body;
    if (!badan) return '';
    const teks = (badan.innerText || '').replace(/[\t ]+/g, ' ').replace(/\n{3,}/g, '\n\n').trim();
    return teks.slice(0, batas || 1500);
  }

  // Keluaran sengaja dibuat datar dan JSON-able: hasilnya dibaca agent lewat
  // Runtime.evaluate, dan struktur yang berisi referensi DOM tidak bisa dikirim.
  function mata(opsi) {
    opsi = opsi || {};
    const batas = Number(opsi.maks) > 0 ? Number(opsi.maks) : 300;
    const mulai = Date.now();
    peta.clear();
    nomorTerakhir = 0;
    const hasil = [];
    const wadah = [];
    jelajah(document.body || document.documentElement, 'dom', wadah, hasil, batas);
    return {
      versi: VERSI,
      url: location.href,
      judul: document.title,
      siap: document.readyState,
      fokus: document.activeElement ? jalur(document.activeElement, 3) : null,
      jumlah: {
        node: document.querySelectorAll('*').length,
        elemen: hasil.length,
        wadah: wadah.length,
        terlihat: hasil.filter((satu) => satu.terlihat).length,
      },
      elemen: hasil,
      wadah: wadah,
      teks: opsi.tanpa_teks ? '' : ringkasanTeks(opsi.batas_teks),
      ms: Date.now() - mulai,
    };
  }

  // Elemen di dalam shadow root tidak terlihat oleh document.querySelectorAll.
  // Pencarian karena itu menelusuri akar yang sama dengan jelajah().
  function cariSemua(selector, akar, keluar, batas) {
    keluar = keluar || [];
    batas = batas || 200;
    if (!akar || keluar.length >= batas) return keluar;
    try {
      if (akar.querySelectorAll) {
        for (const el of akar.querySelectorAll(selector)) {
          keluar.push(el);
          if (keluar.length >= batas) return keluar;
        }
      }
    } catch (_) {
      return keluar;
    }
    const semua = akar.querySelectorAll ? akar.querySelectorAll('*') : [];
    for (const el of semua) {
      const bayangan = el.shadowRoot || akarTertutup.get(el);
      if (bayangan) cariSemua(selector, bayangan, keluar, batas);
      if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {
        let dokumen = null;
        try {
          dokumen = el.contentDocument;
        } catch (_) {}
        if (dokumen && dokumen.body) cariSemua(selector, dokumen.body, keluar, batas);
      }
    }
    return keluar;
  }

  function semuaAkar(akar, keluar) {
    keluar = keluar || [];
    if (!akar) return keluar;
    keluar.push(akar);
    const semua = akar.querySelectorAll ? akar.querySelectorAll('*') : [];
    for (const el of semua) {
      const bayangan = el.shadowRoot || akarTertutup.get(el);
      if (bayangan) semuaAkar(bayangan, keluar);
      if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {
        let dokumen = null;
        try {
          dokumen = el.contentDocument;
        } catch (_) {}
        if (dokumen && dokumen.body) semuaAkar(dokumen.body, keluar);
      }
    }
    return keluar;
  }

  window.__oc = {
    versi: VERSI,
    mata: mata,
    cariSemua: cariSemua,
    semuaAkar: semuaAkar,
    akarTertutup: akarTertutup,
    peran: peran,
    namaAksesibel: namaAksesibel,
    bisaDisentuh: bisaDisentuh,
    terlihat: terlihat,
    jalur: jalur,
    rapikan: rapikan,
    peta: peta,
  };
})();