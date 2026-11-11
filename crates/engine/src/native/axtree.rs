//! Pohon aksesibilitas dari arena [`Document`].
//!
//! `Accessibility.getFullAXTree` adalah satu-satunya cara agen "membaca" isi
//! halaman tanpa penglihatan: teks, peran, dan nama tiap elemen. Jadi pohon
//! ini dibangun dari DOM yang sudah ada, bukan dari raster.
//!
//! Aturan yang dipakai sama dengan yang bisa dipertanggungjawabkan tanpa
//! mesin layout: node teks **tidak** jadi simpul AX sendiri, isinya masuk ke
//! nama induknya (persis seperti Chrome melakukannya). Peran diambil dari peta
//! tag dan atribut ARIA; apa pun yang tidak dikenal jadi `generic`, bukan
//! ditebak.
//!
//! Yang belum ada dan tidak diklaim: perhitungan nama yang memakai
//! `aria-labelledby` berantai lintas dokumen, status *focusable*/*selected*,
//! dan pohon AX untuk konten di dalam `<iframe>` (iframe belum dimuat).

use serde_json::{Value, json};

use super::dom::{Document, NodeId, NodeKind};

/// Satu simpul pohon aksesibilitas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxNode {
    /// Nomor simpul AX, berurutan dari akar.
    pub id: String,
    /// Peran ARIA, mis. `heading`, `link`, `textbox`.
    pub role: String,
    /// Nama yang bisa dibaca agen: teks label, isi elemen, atau atribut.
    pub name: String,
    /// `true` kalau simpul ini tidak boleh dibacakan ke agen.
    pub ignored: bool,
    /// Nomor node DOM asal, untuk menghubungkan AX kembali ke `DOM.*`.
    pub backend: Option<NodeId>,
    pub parent: Option<String>,
    pub children: Vec<String>,
}

/// Tag yang tidak pernah muncul di pohon aksesibilitas.
const TERSEMBUNYI: &[&str] = &[
    "head", "script", "style", "meta", "link", "title", "template", "noscript",
];

/// Peran yang namanya diambil dari isi teks elemen.
const NAMA_DARI_ISI: &[&str] = &[
    "heading",
    "link",
    "button",
    "paragraph",
    "listitem",
    "cell",
    "columnheader",
    "rowheader",
    "LabelText",
    "RootWebArea",
    "main",
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "form",
    "list",
    "table",
    "row",
];

/// Peran dari nama tag HTML.
#[must_use]
pub fn peran_tag(tag: &str) -> &'static str {
    match tag {
        "html" => "RootWebArea",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "p" => "paragraph",
        "a" => "link",
        "button" => "button",
        "img" => "image",
        "ul" | "ol" => "list",
        "li" => "listitem",
        "table" => "table",
        "thead" | "tbody" | "tfoot" => "rowgroup",
        "tr" => "row",
        "td" => "cell",
        "th" => "columnheader",
        "form" => "form",
        "nav" => "navigation",
        "header" => "banner",
        "footer" => "contentinfo",
        "main" => "main",
        "aside" => "complementary",
        "label" => "LabelText",
        "textarea" => "textbox",
        "select" => "combobox",
        "option" => "option",
        "input" | "output" => "textbox",
        "hr" => "separator",
        _ => "generic",
    }
}

/// Peran dari atribut `type` pada `<input>`, karena satu tag bisa berarti
/// kotak teks, tombol, kotak centang, atau radio.
fn peran_input(tipe: Option<&str>) -> &'static str {
    match tipe.map(str::to_ascii_lowercase).as_deref() {
        Some("checkbox") => "checkbox",
        Some("radio") => "radio",
        Some("submit" | "button" | "reset" | "image") => "button",
        Some("range") => "slider",
        Some("number") => "spinbutton",
        Some("search") => "searchbox",
        Some("hidden") => "none",
        _ => "textbox",
    }
}

/// Peran dari atribut `role` eksplisit kalau ada, kalau tidak dari tag.
fn peran(doc: &Document, id: NodeId) -> String {
    let Some(node) = doc.node(id) else {
        return "generic".to_string();
    };
    if let Some(peran) = node.attr("role") {
        let rapi = peran.trim();
        if !rapi.is_empty() {
            return rapi.to_string();
        }
    }
    let Some(tag) = node.tag() else {
        return "generic".to_string();
    };
    if tag == "input" {
        return peran_input(node.attr("type")).to_string();
    }
    peran_tag(tag).to_string()
}

/// Nama yang dibaca agen, berurutan dari yang paling otoritatif.
///
/// Urutannya mengikuti prioritas ARIA: `aria-label`, lalu `alt` untuk gambar,
/// lalu `placeholder` untuk kontrol form, lalu `title`, lalu isi teks elemen
/// untuk peran yang memang dinamai dari isinya.
fn nama(doc: &Document, id: NodeId, role: &str) -> String {
    let Some(node) = doc.node(id) else {
        return String::new();
    };
    for attr in ["aria-label", "alt", "placeholder", "title", "value"] {
        if let Some(v) = node.attr(attr) {
            let rapi = v.trim();
            if !rapi.is_empty() {
                return rapi.to_string();
            }
        }
    }
    if NAMA_DARI_ISI.contains(&role) {
        return doc.teks(id).trim().to_string();
    }
    String::new()
}

/// Apakah elemen ini harus disembunyikan dari agen.
fn terabaikan(doc: &Document, id: NodeId) -> bool {
    let Some(node) = doc.node(id) else {
        return true;
    };
    if let Some(tag) = node.tag()
        && TERSEMBUNYI.contains(&tag)
    {
        return true;
    }
    if node
        .attr("aria-hidden")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    node.attr("hidden").is_some()
}

/// Membangun pohon aksesibilitas datar, urut dari akar.
///
/// Simpul yang `ignored` tetap dikirim: klien CDP memakainya untuk memutuskan
/// apa yang dilewati, dan menyembunyikannya di sini akan membuat nomor simpul
/// tidak bisa dilacak kembali ke DOM.
#[must_use]
pub fn pohon(doc: &Document) -> Vec<AxNode> {
    let mut out: Vec<AxNode> = Vec::new();
    let akar = doc.root();
    let mut anak_dari: Vec<(Option<String>, NodeId)> = Vec::new();
    for &anak in &doc
        .node(akar)
        .map(|n| n.children.clone())
        .unwrap_or_default()
    {
        anak_dari.push((None, anak));
    }

    while let Some((induk, id)) = anak_dari.pop() {
        let Some(node) = doc.node(id) else {
            continue;
        };
        // Node teks bukan simpul AX: isinya sudah masuk ke nama induk.
        if matches!(node.kind, NodeKind::Text(_) | NodeKind::Doctype) {
            continue;
        }
        let role = peran(doc, id);
        let nomor = out.len().to_string();
        let ignored = terabaikan(doc, id);
        let label = nama(doc, id, &role);
        out.push(AxNode {
            id: nomor.clone(),
            role,
            name: label,
            ignored,
            backend: Some(id),
            parent: induk.clone(),
            children: Vec::new(),
        });
        let mut anak: Vec<NodeId> = node.children.clone();
        anak.reverse();
        for c in anak {
            anak_dari.push((Some(nomor.clone()), c));
        }
    }

    // Hubungkan induk ke anak setelah semua nomor diketahui.
    let mut peta: Vec<(Option<String>, usize)> = Vec::with_capacity(out.len());
    for (i, n) in out.iter().enumerate() {
        peta.push((n.parent.clone(), i));
    }
    for (induk, i) in peta {
        let Some(induk) = induk else { continue };
        if let Ok(pos) = induk.parse::<usize>() {
            let anak_id = out[i].id.clone();
            if let Some(p) = out.get_mut(pos) {
                p.children.push(anak_id);
            }
        }
    }
    out
}

/// Bentuk JSON `Accessibility.getFullAXTree`, siap dikirim ke klien.
#[must_use]
pub fn json_pohon(doc: &Document) -> Value {
    let simpul: Vec<Value> = pohon(doc)
        .into_iter()
        .map(|n| {
            let mut obj = json!({
                "nodeId": n.id,
                "ignored": n.ignored,
                "role": { "type": "role", "value": n.role },
                "name": { "type": "computedString", "value": n.name },
                "childIds": n.children,
            });
            if let Some(backend) = n.backend {
                obj["backendDOMNodeId"] = json!(backend);
            }
            if let Some(parent) = n.parent {
                obj["parentId"] = json!(parent);
            }
            obj
        })
        .collect();
    json!({ "nodes": simpul })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cari<'a>(simpul: &'a [AxNode], role: &str, nama: &str) -> &'a AxNode {
        simpul
            .iter()
            .find(|n| n.role == role && n.name == nama)
            .unwrap_or_else(|| panic!("simpul {role} bernama {nama:?} tidak ada: {simpul:#?}"))
    }

    #[test]
    fn hierarki_induk_anak_terhubung() {
        let doc = Document::parse_html("<body><div><h1>Judul</h1><p>isi</p></div></body>").unwrap();
        let simpul = pohon(&doc);
        let judul = cari(&simpul, "heading", "Judul");
        let isi = cari(&simpul, "paragraph", "isi");
        assert_eq!(
            judul.parent, isi.parent,
            "heading dan paragraf adalah anak dari div yang sama"
        );
        let induk = judul.parent.clone().expect("heading punya induk");
        let induk_simpul = simpul
            .iter()
            .find(|n| n.id == induk)
            .expect("induk harus ada di pohon");
        assert_eq!(induk_simpul.role, "generic", "induk keduanya adalah div");
        assert_eq!(
            induk_simpul.children,
            vec![judul.id.clone(), isi.id.clone()]
        );
        assert_eq!(
            simpul.first().map(|n| n.role.as_str()),
            Some("RootWebArea"),
            "akar pohon adalah html"
        );
    }

    #[test]
    fn nama_diambil_dari_atribut_yang_benar() {
        let doc = Document::parse_html(
            "<body><img src=\"a.png\" alt=\"logo\"><input placeholder=\"Cari\">\
             <button aria-label=\"Tutup\">X</button><a href=\"/x\">Beranda</a></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        assert!(cari(&simpul, "image", "logo").backend.is_some());
        assert_eq!(cari(&simpul, "textbox", "Cari").role, "textbox");
        assert_eq!(cari(&simpul, "button", "Tutup").name, "Tutup");
        assert_eq!(cari(&simpul, "link", "Beranda").name, "Beranda");
    }

    #[test]
    fn node_teks_tidak_jadi_simpul_tapi_menamai_induk() {
        let doc = Document::parse_html("<body><p>satu dua</p></body>").unwrap();
        let simpul = pohon(&doc);
        assert!(
            simpul.iter().all(|n| n.role != "text"),
            "tidak ada simpul teks terpisah"
        );
        assert_eq!(cari(&simpul, "paragraph", "satu dua").name, "satu dua");
    }

    #[test]
    fn head_script_dan_aria_hidden_ditandai_ignored() {
        let doc = Document::parse_html(
            "<html><head><title>x</title><script>var a=1</script></head>\
             <body><div aria-hidden=\"true\"><span>rahasia</span></div><p>tampak</p></body></html>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        let tersembunyi = simpul
            .iter()
            .find(|n| n.backend.is_some() && n.ignored && n.role == "generic");
        assert!(
            tersembunyi.is_some(),
            "div aria-hidden harus hadir sebagai ignored"
        );
        assert!(!cari(&simpul, "paragraph", "tampak").ignored);
        assert!(
            simpul.iter().all(|n| n.name != "var a=1"),
            "isi script tidak boleh jadi nama"
        );
    }

    #[test]
    fn peran_input_mengikuti_atribut_type() {
        let doc = Document::parse_html(
            "<body><input type=\"checkbox\"><input type=\"submit\" value=\"Kirim\">\
             <select><option>satu</option></select></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        assert_eq!(cari(&simpul, "checkbox", "").role, "checkbox");
        assert_eq!(cari(&simpul, "button", "Kirim").name, "Kirim");
        assert_eq!(cari(&simpul, "combobox", "").role, "combobox");
    }

    #[test]
    fn json_memuat_field_cdp() {
        let doc = Document::parse_html("<body><h2>Halo</h2></body>").unwrap();
        let isi = json_pohon(&doc);
        let simpul = isi["nodes"].as_array().expect("nodes array");
        let h2 = simpul
            .iter()
            .find(|n| n["role"]["value"] == "heading")
            .expect("heading ada");
        assert_eq!(h2["name"]["value"], "Halo");
        assert_eq!(h2["name"]["type"], "computedString");
        assert!(h2["backendDOMNodeId"].as_u64().is_some());
        assert!(h2["parentId"].is_string());
    }
}
