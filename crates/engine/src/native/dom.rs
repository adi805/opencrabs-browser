//! Pohon DOM arena dari `html5ever`.
//!
//! `markup5ever_rcdom` menyimpan pohon sebagai `Rc<RefCell<..>>`. Itu nyaman
//! untuk parser, tapi tidak bisa diserialisasi, tidak bisa dipindah antar
//! thread, dan tidak bisa disimpan ke `crates/store` untuk tab cold. Jadi
//! pohon Rc diubah sekali menjadi arena `Vec<Node>` berindeks: setiap node
//! punya `parent` dan `children`, dan seluruh dokumen bisa ditulis balik jadi
//! string HTML tanpa kehilangan struktur.
//!
//! Nomor node **stabil** selama dokumen tidak dimutasi. Semua operasi baca
//! memakai [`NodeId`], bukan pointer.

use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

use super::NativeError;

/// Indeks node di dalam arena [`Document`].
pub type NodeId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Doctype,
    /// Nama tag dalam huruf kecil, mis. `div`.
    Element(String),
    Text(String),
    /// Akar pohon bayangan. Bukan elemen: ia tidak punya tag, tidak punya
    /// kotak sendiri, dan tidak pernah muncul di `children` host-nya.
    ShadowRoot {
        mode: ModeBayang,
    },
}

/// Mode shadow root, sesuai `attachShadow({ mode })`.
///
/// Bedanya bukan soal penyimpanan, karena keduanya disimpan sama. Bedanya
/// siapa yang boleh **melihat**: `element.shadowRoot` hanya menjawab untuk
/// mode terbuka, sementara kueri otomasi menembus keduanya. Tanpa pemisahan
/// itu, `mode: "closed"` cuma nama tanpa arti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeBayang {
    /// `mode: "open"`.
    Terbuka,
    /// `mode: "closed"`.
    Tertutup,
}

impl ModeBayang {
    /// Ejaan mode seperti yang dipakai `attachShadow`.
    #[must_use]
    pub fn nama(self) -> &'static str {
        match self {
            Self::Terbuka => "open",
            Self::Tertutup => "closed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    /// Atribut apa adanya dari parser, urutannya dipertahankan.
    pub attrs: Vec<(String, String)>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Shadow root milik node ini, kalau ada. Disimpan di host, bukan di
    /// `children`, karena isi shadow bukan anak pohon terang: `children`,
    /// `textContent`, dan `to_html` harus tetap melihat dokumen tanpa isi
    /// bayangan, sama seperti browser.
    pub bayang: Option<NodeId>,
    /// Dokumen anak hasil muatan `<iframe>` milik node ini, kalau ada.
    /// Disimpan terpisah dari `children` karena `<iframe>` adalah elemen
    /// diganti: isinya tidak ikut `textContent` induk.
    pub bingkai: Option<NodeId>,
}

impl Node {
    /// Nilai atribut, tanpa membedakan besar kecil huruf pada nama.
    pub fn attr(&self, nama: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(nama))
            .map(|(_, v)| v.as_str())
    }

    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Element(tag) => Some(tag),
            _ => None,
        }
    }

    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// Elemen yang tidak punya tag penutup menurut spesifikasi HTML.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

#[derive(Debug, Clone)]
pub struct Document {
    nodes: Vec<Node>,
    root: NodeId,
}

impl Document {
    /// Mengurai HTML menjadi arena. Parser HTML5 tidak pernah menolak input
    /// yang bentuknya rusak, jadi kegagalan di sini hanya bisa berarti galat
    /// I/O saat membaca buffer, dan itu dilaporkan, bukan ditelan.
    pub fn parse_html(html: &str) -> Result<Self, NativeError> {
        let mut input = html.as_bytes();
        let dom = parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut input)
            .map_err(NativeError::parse)?;

        let mut nodes = vec![Node {
            kind: NodeKind::Document,
            attrs: Vec::new(),
            parent: None,
            children: Vec::new(),
            bayang: None,
            bingkai: None,
        }];
        let akar = 0;
        for anak in dom.document.children.borrow().iter() {
            if let Some(id) = bangun(anak, &mut nodes, akar) {
                nodes[akar].children.push(id);
            }
        }
        Ok(Self { nodes, root: akar })
    }

    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root
    }

    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Jumlah node berisi data, yaitu elemen dan teks, yang **bisa dicapai
    /// dari akar**. Node dokumen dan doctype tidak dihitung, supaya angkanya
    /// sebanding dengan pengukuran biaya per tab di `BUKTI-BUDGET`. Node yang
    /// sudah dilepas dari pohon tidak ikut dihitung walau masih menempati
    /// arena: biaya yang menentukan adalah pohon yang hidup, bukan sisa mutasi.
    #[must_use]
    pub fn jumlah_node(&self) -> usize {
        self.id_terpasang()
            .filter(|&i| {
                matches!(
                    &self.nodes[i].kind,
                    NodeKind::Element(_) | NodeKind::Text(_)
                )
            })
            .count()
    }

    #[must_use]
    pub fn jumlah_elemen(&self) -> usize {
        self.id_terpasang()
            .filter(|&i| matches!(&self.nodes[i].kind, NodeKind::Element(_)))
            .count()
    }

    /// `true` kalau node bisa dicapai dari akar dokumen.
    ///
    /// Node yang baru dibuat tapi belum digantungkan, atau yang sudah dilepas,
    /// menjawab `false`. Batas langkah ada supaya pohon yang entah bagaimana
    /// melingkar tidak membuat pemanggil berputar selamanya.
    #[must_use]
    pub fn terpasang(&self, id: NodeId) -> bool {
        let mut kini = Some(id);
        let mut langkah = 0usize;
        while let Some(n) = kini {
            if n == self.root {
                return true;
            }
            langkah += 1;
            if langkah > self.nodes.len() {
                return false;
            }
            kini = self.nodes.get(n).and_then(|node| node.parent);
        }
        false
    }

    fn id_terpasang(&self) -> impl Iterator<Item = NodeId> + '_ {
        (0..self.nodes.len()).filter(|&i| self.terpasang(i))
    }

    /// Pencarian cepat berdasarkan atribut `id`.
    ///
    /// Node yang sudah dilepas dari pohon tidak ditemukan lagi: `id` di DOM
    /// hanya berlaku selama elemennya ada di dokumen.
    ///
    /// Untuk selektor umum, lihat [`Document::pilih`].
    #[must_use]
    pub fn cari_id(&self, id: &str) -> Option<NodeId> {
        self.nodes.iter().enumerate().find_map(|(i, n)| {
            (n.tag().is_some() && n.attr("id") == Some(id) && self.terpasang(i)).then_some(i)
        })
    }

    /// Seluruh teks di dalam sebuah node, digabung tanpa pemisah tambahan.
    #[must_use]
    pub fn teks(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.kumpulkan_teks(id, &mut out);
        out
    }

    fn kumpulkan_teks(&self, id: NodeId, out: &mut String) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        if let NodeKind::Text(t) = &node.kind {
            out.push_str(t);
            return;
        }
        for &anak in &node.children {
            self.kumpulkan_teks(anak, out);
        }
    }

    /// Isi `<title>` pertama, kalau ada.
    #[must_use]
    pub fn judul(&self) -> Option<String> {
        let id = self.nodes.iter().position(|n| n.tag() == Some("title"))?;
        let teks = self.teks(id);
        let rapi = teks.trim();
        if rapi.is_empty() {
            None
        } else {
            Some(rapi.to_string())
        }
    }

    /// HTML luar satu elemen, termasuk turunannya.
    ///
    /// `None` kalau node itu bukan elemen: node teks dan node dokumen tidak
    /// punya tag pembuka, jadi tidak ada "HTML luar" yang bisa ditulis.
    #[must_use]
    pub fn luar_html(&self, id: NodeId) -> Option<String> {
        let node = self.node(id)?;
        node.tag()?;
        let mut out = String::new();
        self.tulis(id, &mut out);
        Some(out)
    }

    /// Isi satu elemen, yaitu yang di browser disebut `innerHTML`.
    ///
    /// Berbeda dari [`Self::luar_html`] yang ikut menulis tag elemennya
    /// sendiri. Isi shadow root tidak ikut: ia bukan anak host di pohon ini,
    /// sama seperti di DOM sungguhan.
    #[must_use]
    pub fn dalam_html(&self, id: NodeId) -> Option<String> {
        self.node(id)?;
        let mut out = String::new();
        for &anak in &self.nodes.get(id)?.children {
            self.tulis(anak, &mut out);
        }
        Some(out)
    }

    /// Menulis dokumen kembali sebagai HTML.
    ///
    /// Dipakai oleh snapshot tab cold: bentuk inilah yang disimpan, dan
    /// [`Document::parse_html`] menerimanya kembali. Atribut ditulis apa
    /// adanya, teks di-escape.
    #[must_use]
    pub fn to_html(&self) -> String {
        let mut out = String::new();
        for &anak in &self.nodes[self.root].children {
            self.tulis(anak, &mut out);
        }
        out
    }

    fn tulis(&self, id: NodeId, out: &mut String) {
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        match &node.kind {
            NodeKind::Document => {
                for &anak in &node.children {
                    self.tulis(anak, out);
                }
            }
            NodeKind::Doctype => out.push_str("<!DOCTYPE html>"),
            // Isi shadow root tidak pernah ada di `children` host-nya, jadi
            // jalur ini tidak tercapai dari `to_html`. Ditulis eksplisit
            // supaya `match` tetap lengkap dan pembaca tidak mengira isi
            // bayangan ikut diserialisasi.
            NodeKind::ShadowRoot { .. } => {}
            NodeKind::Text(t) => escape_teks(t, out),
            NodeKind::Element(tag) => {
                out.push('<');
                out.push_str(tag);
                for (k, v) in &node.attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    escape_atribut(v, out);
                    out.push('"');
                }
                out.push('>');
                if VOID.contains(&tag.as_str()) {
                    return;
                }
                for &anak in &node.children {
                    self.tulis(anak, out);
                }
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
}

/// Mutasi pohon. `NodeId` tetap stabil seumur `Document`: node yang dilepas
/// tidak dihapus dari arena, hanya dilepas dari induknya, sehingga nomor node
/// yang sudah dipegang pemanggil lain tidak bergeser. Arena yang tumbuh itu
/// disengaja; yang menentukan biaya per tab adalah jumlah node yang bisa
/// dicapai dari akar, dan itulah yang dihitung [`Document::jumlah_node`].
impl Document {
    /// Membuat elemen baru tanpa induk dan tanpa anak.
    ///
    /// Nama tag disimpan huruf kecil, sama seperti hasil parser: HTML tidak
    /// membedakan `DIV` dan `div`, jadi keduanya harus menghasilkan node yang
    /// sama.
    pub fn buat_elemen(&mut self, tag: &str) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind: NodeKind::Element(tag.to_ascii_lowercase()),
            attrs: Vec::new(),
            parent: None,
            children: Vec::new(),
            bayang: None,
            bingkai: None,
        });
        id
    }

    /// Membuat node teks tanpa induk.
    pub fn buat_teks(&mut self, teks: &str) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind: NodeKind::Text(teks.to_owned()),
            attrs: Vec::new(),
            parent: None,
            children: Vec::new(),
            bayang: None,
            bingkai: None,
        });
        id
    }

    /// Menggantungkan `anak` di bawah `induk`, melepasnya dari induk lama.
    ///
    /// Ditolak kalau node tidak ada, `anak` adalah akar dokumen, `anak` adalah
    /// `induk` itu sendiri, `induk` bukan wadah (node teks tidak punya anak),
    /// atau `induk` berada di dalam `anak` sehingga terbentuk siklus. Mutasi
    /// yang ditolak tidak mengubah pohon sama sekali.
    pub fn tambah_anak(&mut self, induk: NodeId, anak: NodeId) -> Result<(), NativeError> {
        self.pasangkan(induk, anak, None)
    }

    /// Menyisipkan `anak` sebelum `acuan` di bawah `induk`.
    ///
    /// `acuan` wajib benar-benar anak dari `induk`; kalau tidak, mutasinya
    /// ditolak alih-alih menebak posisi.
    pub fn sisip_sebelum(
        &mut self,
        induk: NodeId,
        anak: NodeId,
        acuan: NodeId,
    ) -> Result<(), NativeError> {
        self.pasangkan(induk, anak, Some(acuan))
    }

    fn pasangkan(
        &mut self,
        induk: NodeId,
        anak: NodeId,
        acuan: Option<NodeId>,
    ) -> Result<(), NativeError> {
        let Some(induk_node) = self.nodes.get(induk) else {
            return Err(NativeError::dom(format!("induk {induk} tidak ada")));
        };
        if !matches!(
            &induk_node.kind,
            NodeKind::Document | NodeKind::Element(_) | NodeKind::ShadowRoot { .. }
        ) {
            return Err(NativeError::dom(format!(
                "node {induk} bukan wadah, tidak bisa punya anak"
            )));
        }
        if anak == self.root {
            return Err(NativeError::dom("akar dokumen tidak bisa dipindah"));
        }
        if anak == induk {
            return Err(NativeError::dom(
                "node tidak bisa menjadi anak dirinya sendiri",
            ));
        }
        match self.nodes.get(anak) {
            None => return Err(NativeError::dom(format!("anak {anak} tidak ada"))),
            Some(n) if matches!(&n.kind, NodeKind::Document) => {
                return Err(NativeError::dom("node dokumen tidak bisa dipindah"));
            }
            Some(_) => {}
        }
        if self.leluhur_dari(induk).contains(&anak) {
            return Err(NativeError::dom(format!(
                "siklus ditolak: {induk} berada di dalam {anak}"
            )));
        }
        if let Some(acuan_id) = acuan
            && !self.nodes[induk].children.contains(&acuan_id)
        {
            return Err(NativeError::dom(format!(
                "acuan {acuan_id} bukan anak dari {induk}"
            )));
        }

        self.lepas_dari_induk(anak);
        self.nodes[anak].parent = Some(induk);
        let posisi = match acuan {
            Some(acuan_id) => self.nodes[induk]
                .children
                .iter()
                .position(|&c| c == acuan_id)
                .unwrap_or(self.nodes[induk].children.len()),
            None => self.nodes[induk].children.len(),
        };
        self.nodes[induk].children.insert(posisi, anak);
        Ok(())
    }

    /// Melepas `id` dari induknya. `true` kalau node itu memang punya induk.
    ///
    /// Nomor node dan isinya tetap ada; yang hilang hanya sambungannya ke
    /// pohon. Node yang dilepas tidak lagi muncul di `to_html`, tidak ditemukan
    /// `cari_id`, dan tidak dihitung `jumlah_node`.
    pub fn lepas(&mut self, id: NodeId) -> bool {
        let punya_induk = self.nodes.get(id).is_some_and(|n| n.parent.is_some());
        if punya_induk {
            self.lepas_dari_induk(id);
        }
        punya_induk
    }

    /// Menetapkan atribut. Nama yang sudah ada diganti di tempatnya supaya
    /// urutan atribut tidak berubah tiap kali nilainya diperbarui; nama baru
    /// ditambahkan di belakang, sama seperti `setAttribute` di DOM.
    pub fn set_atribut(&mut self, id: NodeId, nama: &str, nilai: &str) -> Result<(), NativeError> {
        let Some(node) = self.nodes.get_mut(id) else {
            return Err(NativeError::dom(format!("node {id} tidak ada")));
        };
        if !matches!(&node.kind, NodeKind::Element(_)) {
            return Err(NativeError::dom(format!(
                "node {id} bukan elemen, tidak punya atribut"
            )));
        }
        match node
            .attrs
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(nama))
        {
            Some(slot) => slot.1 = nilai.to_owned(),
            None => node.attrs.push((nama.to_owned(), nilai.to_owned())),
        }
        Ok(())
    }

    /// Menghapus atribut. `true` kalau atribut itu memang ada.
    pub fn hapus_atribut(&mut self, id: NodeId, nama: &str) -> bool {
        let Some(node) = self.nodes.get_mut(id) else {
            return false;
        };
        let sebelum = node.attrs.len();
        node.attrs.retain(|(k, _)| !k.eq_ignore_ascii_case(nama));
        node.attrs.len() != sebelum
    }

    /// Mengganti isi teks sebuah node.
    ///
    /// Untuk node teks, isinya diganti di tempat. Untuk elemen, seluruh anaknya
    /// dibuang dan diganti satu node teks, sama seperti `textContent = ...` di
    /// DOM. Node dokumen dan doctype ditolak: keduanya tidak punya isi teks.
    pub fn set_teks(&mut self, id: NodeId, teks: &str) -> Result<(), NativeError> {
        let Some(node) = self.nodes.get(id) else {
            return Err(NativeError::dom(format!("node {id} tidak ada")));
        };
        let node_teks = matches!(&node.kind, NodeKind::Text(_));
        let node_elemen = matches!(&node.kind, NodeKind::Element(_));
        if node_teks {
            self.nodes[id].kind = NodeKind::Text(teks.to_owned());
            return Ok(());
        }
        if !node_elemen {
            return Err(NativeError::dom(format!("node {id} tidak punya isi teks")));
        }
        for anak in std::mem::take(&mut self.nodes[id].children) {
            self.nodes[anak].parent = None;
        }
        let baru = self.buat_teks(teks);
        self.tambah_anak(id, baru)
    }

    /// Semua leluhur `id`, termasuk `id` itu sendiri, dari yang terdekat.
    ///
    /// Batas langkah memakai jumlah node: pohon yang melingkar akan berhenti
    /// di sini, bukan berputar selamanya.
    fn leluhur_dari(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut kini = Some(id);
        while let Some(n) = kini {
            if out.len() > self.nodes.len() {
                break;
            }
            out.push(n);
            kini = self.nodes.get(n).and_then(|node| node.parent);
        }
        out
    }

    fn lepas_dari_induk(&mut self, id: NodeId) {
        if let Some(induk) = self.nodes.get(id).and_then(|n| n.parent) {
            if let Some(anak) = self.nodes.get_mut(induk) {
                anak.children.retain(|&c| c != id);
            }
            self.nodes[id].parent = None;
        }
    }
}

/// Pohon bayangan dan dokumen `<iframe>`.
///
/// Keduanya sama-sama pohon yang **tidak** ada di `children` host-nya, dan
/// keduanya disimpan di arena yang sama. Itu pilihan sadar: nomor node tetap
/// global, jadi selektor, layout, dan pohon aksesibilitas bisa menelusuri
/// keduanya tanpa tipe baru di mana-mana. Yang membedakan hanya siapa yang
/// boleh melihat: kueri otomasi menembus batas, skrip halaman tidak.
impl Document {
    /// Memasang shadow root di bawah `host`.
    ///
    /// Ditolak kalau `host` bukan elemen atau sudah punya shadow root: satu
    /// host hanya boleh punya satu, dan `attachShadow` kedua di browser
    /// melempar galat, bukan mengganti yang lama.
    pub fn pasang_bayang(&mut self, host: NodeId, mode: ModeBayang) -> Result<NodeId, NativeError> {
        match self.nodes.get(host) {
            None => return Err(NativeError::dom(format!("host {host} tidak ada"))),
            Some(n) if n.tag().is_none() => {
                return Err(NativeError::dom(format!("node {host} bukan elemen")));
            }
            Some(n) if n.bayang.is_some() => {
                return Err(NativeError::dom(format!(
                    "node {host} sudah punya shadow root"
                )));
            }
            Some(_) => {}
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind: NodeKind::ShadowRoot { mode },
            attrs: Vec::new(),
            parent: Some(host),
            children: Vec::new(),
            bayang: None,
            bingkai: None,
        });
        self.nodes[host].bayang = Some(id);
        Ok(id)
    }

    /// Shadow root milik `host`, **apa pun mode-nya**.
    ///
    /// Ini pintu otomasi, dan sengaja tidak memeriksa mode: `AC-016` meminta
    /// root tertutup tetap bisa ditembus dari sisi otomasi. Yang dijaga
    /// terpisah adalah jalur skrip halaman, [`Self::bayang_terbuka`].
    #[must_use]
    pub fn bayang(&self, host: NodeId) -> Option<NodeId> {
        self.nodes.get(host)?.bayang
    }

    /// Shadow root yang boleh dilihat skrip halaman, yaitu yang `open`.
    #[must_use]
    pub fn bayang_terbuka(&self, host: NodeId) -> Option<NodeId> {
        let id = self.bayang(host)?;
        matches!(
            self.nodes.get(id)?.kind,
            NodeKind::ShadowRoot {
                mode: ModeBayang::Terbuka
            }
        )
        .then_some(id)
    }

    /// Mode sebuah shadow root, atau `None` kalau node itu bukan shadow root.
    #[must_use]
    pub fn mode_bayang(&self, id: NodeId) -> Option<ModeBayang> {
        match self.nodes.get(id)?.kind {
            NodeKind::ShadowRoot { mode } => Some(mode),
            _ => None,
        }
    }

    /// Memasang dokumen anak hasil muatan `<iframe>`.
    ///
    /// `html` di-parse sebagai dokumen tersendiri lalu digabung ke arena ini
    /// dengan nomor node yang digeser, sehingga nomor yang sudah dipegang
    /// pemanggil tidak berubah. Node akarnya tetap bertipe [`NodeKind::Document`],
    /// jadi batas dokumen tetap terlihat oleh [`Self::akar_pohon`]: skrip
    /// halaman induk tidak bisa menyeberang ke dalamnya.
    pub fn pasang_bingkai(&mut self, iframe: NodeId, html: &str) -> Result<NodeId, NativeError> {
        match self.nodes.get(iframe) {
            None => return Err(NativeError::dom(format!("iframe {iframe} tidak ada"))),
            Some(n) if n.tag() != Some("iframe") => {
                return Err(NativeError::dom(format!("node {iframe} bukan <iframe>")));
            }
            Some(n) if n.bingkai.is_some() => {
                return Err(NativeError::dom(format!(
                    "iframe {iframe} sudah punya dokumen"
                )));
            }
            Some(_) => {}
        }
        let anak = Document::parse_html(html)?;
        let geser = self.nodes.len();
        let akar = geser + anak.root;
        for (i, mut node) in anak.nodes.into_iter().enumerate() {
            node.parent = if i == anak.root {
                Some(iframe)
            } else {
                node.parent.map(|p| p + geser)
            };
            node.children = node.children.iter().map(|&c| c + geser).collect();
            node.bayang = node.bayang.map(|b| b + geser);
            node.bingkai = node.bingkai.map(|b| b + geser);
            self.nodes.push(node);
        }
        self.nodes[iframe].bingkai = Some(akar);
        Ok(akar)
    }

    /// Dokumen anak milik `<iframe>`, kalau sudah dipasang.
    #[must_use]
    pub fn bingkai(&self, iframe: NodeId) -> Option<NodeId> {
        self.nodes.get(iframe)?.bingkai
    }

    /// Anak yang ikut ditata dan dilukis: anak pohon terang, lalu isi shadow
    /// root kalau ada.
    ///
    /// Node shadow root sendiri tidak ikut: ia bukan elemen dan tidak punya
    /// kotak, jadi memasukkannya ke taffy hanya menambah kotak nol yang tidak
    /// bisa dipakai siapa pun.
    #[must_use]
    pub fn anak_tata(&self, id: NodeId) -> Vec<NodeId> {
        let Some(node) = self.nodes.get(id) else {
            return Vec::new();
        };
        let mut out = node.children.clone();
        if let Some(bayang) = node.bayang
            && let Some(akar) = self.nodes.get(bayang)
        {
            out.extend(akar.children.iter().copied());
        }
        out
    }

    /// Akar pohon milik `id`: node dokumen terdekat, atau shadow root terdekat.
    ///
    /// Dua node berada di pohon yang sama kalau akarnya sama. Itu definisi
    /// yang dipakai untuk memutuskan apa yang boleh dilihat skrip halaman,
    /// jadi ia harus dihitung dari sambungan `parent`, bukan dari jenis node.
    #[must_use]
    pub fn akar_pohon(&self, id: NodeId) -> NodeId {
        let mut kini = id;
        let mut langkah = 0usize;
        loop {
            match self.nodes.get(kini) {
                Some(n) if matches!(n.kind, NodeKind::Document | NodeKind::ShadowRoot { .. }) => {
                    return kini;
                }
                Some(n) => match n.parent {
                    Some(p) if langkah <= self.nodes.len() => {
                        kini = p;
                        langkah += 1;
                    }
                    _ => return kini,
                },
                None => return kini,
            }
        }
    }

    /// `true` kalau `id` bisa dicapai skrip halaman dari akar dokumen: tidak
    /// di balik batas shadow root, dan tidak di dalam dokumen `<iframe>`.
    #[must_use]
    pub fn terlihat_skrip(&self, id: NodeId) -> bool {
        self.akar_pohon(id) == self.root
    }
}

fn bangun(handle: &Handle, nodes: &mut Vec<Node>, induk: NodeId) -> Option<NodeId> {
    let (kind, attrs) = match &handle.data {
        NodeData::Element { name, attrs, .. } => (
            NodeKind::Element(name.local.to_string()),
            attrs
                .borrow()
                .iter()
                .map(|a| (a.name.local.to_string(), a.value.to_string()))
                .collect(),
        ),
        NodeData::Text { contents } => (NodeKind::Text(contents.borrow().to_string()), Vec::new()),
        NodeData::Doctype { .. } => (NodeKind::Doctype, Vec::new()),
        _ => return None,
    };

    let id = nodes.len();
    nodes.push(Node {
        kind,
        attrs,
        parent: Some(induk),
        children: Vec::new(),
        bayang: None,
        bingkai: None,
    });

    for anak in handle.children.borrow().iter() {
        if let Some(c) = bangun(anak, nodes, id) {
            nodes[id].children.push(c);
        }
    }
    Some(id)
}

fn escape_teks(teks: &str, out: &mut String) {
    for c in teks.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_atribut(nilai: &str, out: &mut String) {
    for c in nilai.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mengurai_html_dan_menghitung_node() {
        let doc = Document::parse_html("<p id=\"a\">halo</p>").unwrap();
        // html5ever selalu membentuk html, head, body di sekitar fragmen.
        assert_eq!(doc.jumlah_elemen(), 4, "html, head, body, p");
        assert_eq!(doc.jumlah_node(), 5, "empat elemen plus satu node teks");
    }

    #[test]
    fn mencari_node_lewat_atribut_id() {
        let doc = Document::parse_html("<div><span id=\"target\">x</span></div>").unwrap();
        let id = doc.cari_id("target").expect("id harus ketemu");
        assert_eq!(doc.node(id).unwrap().tag(), Some("span"));
        assert_eq!(doc.teks(id), "x");
    }

    #[test]
    fn serialisasi_bisa_diurai_ulang() {
        let html =
            "<div class=\"kartu\" id=\"k1\"><h2>Judul &amp; isi</h2><input name=\"f\"></div>";
        let doc = Document::parse_html(html).unwrap();
        let lagi = Document::parse_html(&doc.to_html()).unwrap();
        assert_eq!(doc.jumlah_node(), lagi.jumlah_node());
        assert_eq!(doc.to_html(), lagi.to_html());
        assert_eq!(lagi.judul(), None);
    }

    #[test]
    fn judul_dibaca_dari_head() {
        let doc = Document::parse_html("<html><head><title> uji </title></head></html>").unwrap();
        assert_eq!(doc.judul().as_deref(), Some("uji"));
    }

    #[test]
    fn html_rusak_tetap_diurai() {
        let doc = Document::parse_html("<div><p>tanpa penutup").unwrap();
        assert!(doc.jumlah_elemen() >= 2);
    }

    #[test]
    fn luar_html_hanya_untuk_elemen() {
        let doc = Document::parse_html("<div class=\"k\"><p>x</p></div>").unwrap();
        let div = doc.pilih_satu("div").unwrap().expect("div harus ada");
        assert_eq!(
            doc.luar_html(div).as_deref(),
            Some("<div class=\"k\"><p>x</p></div>")
        );
        assert!(
            doc.luar_html(doc.root()).is_none(),
            "node dokumen tidak punya HTML luar"
        );
    }

    // ── Mutasi ────────────────────────────────────────────────────────────
    // W4a. Sebelum ini `Document` hanya bisa dibaca: pohon lahir dari parser
    // dan tidak pernah berubah. Skrip halaman yang memanggil
    // `document.createElement` lalu `appendChild` butuh pohon yang bisa
    // ditulis, dan tanpa tes ini "sudah bisa dimutasi" hanya klaim.

    #[test]
    fn mutasi_elemen_baru_muncul_di_html() {
        let mut doc = Document::parse_html("<body></body>").unwrap();
        let body = doc.pilih_satu("body").unwrap().expect("body harus ada");
        let div = doc.buat_elemen("DIV");
        doc.set_atribut(div, "id", "kotak").unwrap();
        let teks = doc.buat_teks("isi");
        doc.tambah_anak(div, teks).unwrap();
        doc.tambah_anak(body, div).unwrap();

        assert_eq!(
            doc.luar_html(div).as_deref(),
            Some("<div id=\"kotak\">isi</div>")
        );
        // HTML tidak membedakan DIV dan div, jadi node-nya harus sama.
        assert_eq!(doc.node(div).unwrap().tag(), Some("div"));
        assert_eq!(doc.cari_id("kotak"), Some(div));
    }

    #[test]
    fn memindah_anak_melepas_induk_lama() {
        let mut doc =
            Document::parse_html("<div id=\"a\"><span id=\"s\">x</span></div><div id=\"b\"></div>")
                .unwrap();
        let a = doc.cari_id("a").unwrap();
        let b = doc.cari_id("b").unwrap();
        let s = doc.cari_id("s").unwrap();

        doc.tambah_anak(b, s).unwrap();

        assert_eq!(doc.node(s).unwrap().parent, Some(b));
        assert!(!doc.node(a).unwrap().children.contains(&s));
        assert!(doc.node(b).unwrap().children.contains(&s));
        assert_eq!(doc.teks(b), "x");
        assert_eq!(doc.teks(a), "");
    }

    #[test]
    fn siklus_ditolak_tanpa_mengubah_pohon() {
        let mut doc =
            Document::parse_html("<div id=\"luar\"><div id=\"dalam\"></div></div>").unwrap();
        let luar = doc.cari_id("luar").unwrap();
        let dalam = doc.cari_id("dalam").unwrap();
        let induk_luar = doc.node(luar).unwrap().parent;

        let galat = doc.tambah_anak(dalam, luar);

        assert!(galat.is_err(), "siklus harus ditolak");
        assert!(doc.node(dalam).unwrap().children.is_empty());
        assert_eq!(doc.node(luar).unwrap().parent, induk_luar);
    }

    #[test]
    fn node_teks_tidak_bisa_jadi_wadah() {
        let mut doc = Document::parse_html("<p id=\"p\">halo</p>").unwrap();
        let p = doc.cari_id("p").unwrap();
        let anak_teks = doc.node(p).unwrap().children[0];
        let lain = doc.buat_teks("x");

        assert!(
            doc.tambah_anak(anak_teks, lain).is_err(),
            "node teks tidak punya anak"
        );
    }

    #[test]
    fn akar_dokumen_tidak_bisa_dipindah() {
        let mut doc = Document::parse_html("<p>x</p>").unwrap();
        let p = doc.pilih_satu("p").unwrap().expect("p harus ada");
        let akar = doc.root();

        assert!(doc.tambah_anak(p, akar).is_err());
    }

    #[test]
    fn set_teks_mengganti_seluruh_isi_elemen() {
        let mut doc = Document::parse_html("<p id=\"p\"><b>tebal</b> biasa</p>").unwrap();
        let p = doc.cari_id("p").unwrap();

        doc.set_teks(p, "baru").unwrap();

        assert_eq!(doc.teks(p), "baru");
        assert_eq!(doc.node(p).unwrap().children.len(), 1);
        assert_eq!(doc.luar_html(p).as_deref(), Some("<p id=\"p\">baru</p>"));
    }

    #[test]
    fn set_teks_pada_node_teks_mengganti_di_tempat() {
        let mut doc = Document::parse_html("<p id=\"p\">lama</p>").unwrap();
        let p = doc.cari_id("p").unwrap();
        let teks = doc.node(p).unwrap().children[0];

        doc.set_teks(teks, "baru").unwrap();

        assert_eq!(doc.node(teks).unwrap().children.len(), 0);
        assert_eq!(doc.teks(p), "baru");
    }

    #[test]
    fn node_yang_dilepas_keluar_dari_pohon_tapi_nomornya_tetap() {
        let mut doc = Document::parse_html("<div id=\"a\"><span>x</span></div>").unwrap();
        let a = doc.cari_id("a").unwrap();
        let span = doc.node(a).unwrap().children[0];
        let sebelum = doc.jumlah_node();

        assert!(doc.lepas(span));

        assert_eq!(
            doc.jumlah_node(),
            sebelum - 2,
            "span dan node teksnya keluar dari pohon"
        );
        assert!(!doc.terpasang(span));
        assert_eq!(doc.luar_html(a).as_deref(), Some("<div id=\"a\"></div>"));
        // Nomor node stabil: node yang dilepas masih bisa dibaca.
        assert_eq!(doc.node(span).unwrap().tag(), Some("span"));
        // Melepas dua kali bukan galat, tapi juga bukan perubahan.
        assert!(!doc.lepas(span));
    }

    #[test]
    fn node_yang_dilepas_tidak_ditemukan_cari_id() {
        let mut doc = Document::parse_html("<div id=\"a\"><span id=\"s\">x</span></div>").unwrap();
        let a = doc.cari_id("a").unwrap();
        let s = doc.cari_id("s").unwrap();

        assert!(doc.lepas(s));
        assert_eq!(doc.cari_id("s"), None);
        assert_eq!(doc.cari_id("a"), Some(a));
    }

    #[test]
    fn sisip_sebelum_menaruh_di_posisi_yang_benar() {
        let mut doc = Document::parse_html("<div id=\"d\"><i>1</i><i>2</i></div>").unwrap();
        let d = doc.cari_id("d").unwrap();
        let acuan = doc.node(d).unwrap().children[1];
        let baru = doc.buat_elemen("b");

        doc.sisip_sebelum(d, baru, acuan).unwrap();

        let anak = doc.node(d).unwrap().children.clone();
        assert_eq!(anak.len(), 3);
        assert_eq!(doc.node(anak[1]).unwrap().tag(), Some("b"));
        assert_eq!(anak[2], acuan);
    }

    #[test]
    fn sisip_sebelum_acuan_asing_ditolak() {
        let mut doc =
            Document::parse_html("<div id=\"a\"></div><div id=\"b\"><i>x</i></div>").unwrap();
        let a = doc.cari_id("a").unwrap();
        let b = doc.cari_id("b").unwrap();
        let asing = doc.node(b).unwrap().children[0];
        let baru = doc.buat_elemen("p");

        assert!(doc.sisip_sebelum(a, baru, asing).is_err());
        assert!(doc.node(a).unwrap().children.is_empty());
    }

    #[test]
    fn hapus_atribut_mengembalikan_apakah_ada() {
        let mut doc = Document::parse_html("<div id=\"a\" class=\"k\"></div>").unwrap();
        let a = doc.cari_id("a").unwrap();

        doc.set_atribut(a, "data-x", "1").unwrap();
        assert_eq!(doc.node(a).unwrap().attr("data-x"), Some("1"));
        // Nama atribut di HTML tidak membedakan besar kecil huruf.
        assert_eq!(doc.node(a).unwrap().attr("DATA-X"), Some("1"));

        assert!(doc.hapus_atribut(a, "CLASS"));
        assert!(!doc.hapus_atribut(a, "class"));
        assert_eq!(doc.node(a).unwrap().attr("class"), None);
    }

    #[test]
    fn set_atribut_pada_node_teks_ditolak() {
        let mut doc = Document::parse_html("<p>halo</p>").unwrap();
        let teks = doc
            .node(doc.pilih_satu("p").unwrap().unwrap())
            .unwrap()
            .children[0];

        assert!(doc.set_atribut(teks, "id", "x").is_err());
    }

    #[test]
    fn mutasi_bertahan_lewat_serialisasi() {
        let mut doc = Document::parse_html("<body></body>").unwrap();
        let body = doc.pilih_satu("body").unwrap().unwrap();
        let ul = doc.buat_elemen("ul");
        for isi in ["satu", "dua"] {
            let li = doc.buat_elemen("li");
            doc.set_teks(li, isi).unwrap();
            doc.tambah_anak(ul, li).unwrap();
        }
        doc.tambah_anak(body, ul).unwrap();

        let html = doc.to_html();
        let lagi = Document::parse_html(&html).unwrap();

        assert_eq!(lagi.to_html(), html);
        assert_eq!(lagi.jumlah_node(), doc.jumlah_node());
        assert_eq!(
            lagi.teks(lagi.pilih_satu("ul").unwrap().unwrap()),
            "satudua"
        );
    }

    #[test]
    fn shadow_root_tidak_masuk_pohon_terang_tapi_tercapai_otomasi() {
        let mut doc = Document::parse_html(r#"<div id="host"><span>terang</span></div>"#).unwrap();
        let host = doc.cari_id("host").unwrap();
        let akar = doc.pasang_bayang(host, ModeBayang::Terbuka).unwrap();
        let dalam = doc.buat_elemen("b");
        doc.set_teks(dalam, "bayang").unwrap();
        doc.tambah_anak(akar, dalam).unwrap();

        // Pohon terang tidak berubah: isi bayangan bukan anak host, jadi
        // `textContent` host tidak ikut membawa teks bayangan.
        assert_eq!(doc.node(host).unwrap().children.len(), 1);
        assert_eq!(doc.teks(host), "terang");
        assert!(!doc.to_html().contains("bayang"));

        // Tapi ia tetap tercapai dari host lewat `anak_tata`: anak terang
        // dulu, lalu isi shadow root. Urutan itu yang membuat layout dan
        // raster melihat keduanya tanpa pohon kedua.
        let terang = doc.node(host).unwrap().children[0];
        assert_eq!(doc.anak_tata(host), vec![terang, dalam]);
        assert!(doc.terpasang(dalam));
        assert_eq!(doc.akar_pohon(dalam), akar);
    }

    #[test]
    fn shadow_root_tertutup_hanya_terbuka_bagi_otomasi() {
        let mut doc = Document::parse_html(r#"<div id="host"></div>"#).unwrap();
        let host = doc.cari_id("host").unwrap();
        let akar = doc.pasang_bayang(host, ModeBayang::Tertutup).unwrap();

        // Pintu otomasi tidak memeriksa mode; pintu skrip halaman memeriksa.
        assert_eq!(doc.bayang(host), Some(akar));
        assert_eq!(doc.bayang_terbuka(host), None);
        assert_eq!(doc.mode_bayang(akar), Some(ModeBayang::Tertutup));

        // Shadow root kedua ditolak, bukan menggantikan yang lama.
        assert!(doc.pasang_bayang(host, ModeBayang::Terbuka).is_err());
        assert_eq!(doc.bayang(host), Some(akar));
    }

    #[test]
    fn shadow_root_ditolak_pada_node_bukan_elemen() {
        let mut doc = Document::parse_html("<p>teks</p>").unwrap();
        let teks = doc
            .node(doc.pilih_satu("p").unwrap().unwrap())
            .unwrap()
            .children[0];
        assert!(doc.pasang_bayang(teks, ModeBayang::Terbuka).is_err());
        assert!(doc.pasang_bayang(doc.root(), ModeBayang::Terbuka).is_err());
    }

    #[test]
    fn bingkai_iframe_menggeser_nomor_tanpa_menggeser_nomor_lama() {
        let mut doc = Document::parse_html(r#"<body><iframe id="f"></iframe></body>"#).unwrap();
        let iframe = doc.cari_id("f").unwrap();
        let sebelum = doc.nodes().len();

        let akar = doc
            .pasang_bingkai(iframe, r#"<p id="dalam">isi anak</p>"#)
            .unwrap();

        // Nomor yang sudah dipegang pemanggil tidak bergeser, dan dokumen
        // anak mulai tepat di ujung arena yang lama.
        assert_eq!(akar, sebelum);
        assert_eq!(doc.cari_id("f"), Some(iframe));
        assert_eq!(doc.bingkai(iframe), Some(akar));

        // Batas dokumen tetap terlihat: isi iframe bukan pohon skrip induk.
        let dalam = doc.cari_id("dalam").unwrap();
        assert_eq!(doc.akar_pohon(dalam), akar);
        assert!(!doc.terlihat_skrip(dalam));
        assert_eq!(doc.teks(akar), "isi anak");

        // `<iframe>` elemen diganti: isinya tidak ikut `textContent` induk.
        assert_eq!(doc.teks(iframe), "");
        assert!(!doc.to_html().contains("isi anak"));

        // Satu iframe satu dokumen.
        assert!(doc.pasang_bingkai(iframe, "<p>lagi</p>").is_err());
        // Node selain `<iframe>` ditolak.
        let body = doc.pilih_satu("body").unwrap().unwrap();
        assert!(doc.pasang_bingkai(body, "<p>x</p>").is_err());
    }
}
