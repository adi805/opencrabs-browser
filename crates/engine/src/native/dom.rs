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
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    /// Atribut apa adanya dari parser, urutannya dipertahankan.
    pub attrs: Vec<(String, String)>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
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

    /// Jumlah node berisi data, yaitu elemen dan teks. Node dokumen dan
    /// doctype tidak dihitung, supaya angkanya sebanding dengan pengukuran
    /// biaya per tab di `BUKTI-BUDGET`.
    #[must_use]
    pub fn jumlah_node(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Element(_) | NodeKind::Text(_)))
            .count()
    }

    #[must_use]
    pub fn jumlah_elemen(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Element(_)))
            .count()
    }

    /// Pencarian cepat berdasarkan atribut `id`.
    ///
    /// Untuk selektor umum, lihat [`Document::pilih`].
    #[must_use]
    pub fn cari_id(&self, id: &str) -> Option<NodeId> {
        self.nodes
            .iter()
            .position(|n| n.tag().is_some() && n.attr("id") == Some(id))
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
}
