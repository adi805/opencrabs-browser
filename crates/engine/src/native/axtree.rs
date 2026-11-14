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
//! Yang belum ada dan tidak diklaim: pohon AX untuk konten di dalam
//! `<iframe>`. Iframe belum dimuat sama sekali (`html5ever` hanya mem-parse
//! dokumen induk), jadi tidak ada dokumen anak yang bisa dijadikan pohon;
//! itu AC-015 dan dikerjakan bersama penelusuran iframe, bukan di sini.

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
    /// Bisa menerima fokus lewat Tab atau `element.focus()`.
    pub focusable: bool,
    /// Sedang memegang fokus.
    ///
    /// Selalu `false` hari ini: mesin ini belum punya state fokus
    /// (`focus()` mengirim peristiwanya tanpa memindahkan `activeElement`),
    /// jadi menebaknya dari `autofocus` akan mengarang keadaan yang tidak
    /// pernah benar-benar terjadi. Fieldnya tetap ada supaya bentuk CDP-nya
    /// tidak berubah saat state fokus menyusul.
    pub focused: bool,
    /// `aria-selected` atau `<option selected>`; `None` kalau tidak berlaku.
    pub selected: Option<bool>,
    /// `aria-expanded`; `None` kalau elemennya tidak bisa dibuka-tutup.
    pub expanded: Option<bool>,
    /// `disabled` atau `aria-disabled`.
    pub disabled: bool,
    /// `aria-checked`, `checked` pada checkbox/radio, atau `aria-pressed`.
    pub checked: Option<bool>,
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
    "option",
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
/// Urutannya mengikuti prioritas ARIA: `aria-labelledby`, lalu `aria-label`,
/// lalu `alt` untuk gambar, lalu `placeholder` untuk kontrol form, lalu
/// `title`, lalu isi teks elemen untuk peran yang memang dinamai dari isinya.
///
/// `aria-labelledby` adalah yang pertama karena itu satu-satunya atribut yang
/// menunjuk elemen LAIN sebagai sumber nama; kalau operator menulisnya, ia
/// lebih tahu daripada teks di dalam elemen ini. Nilainya daftar id dipisah
/// spasi, dan namanya digabung dengan spasi, persis seperti accname.
fn nama(doc: &Document, id: NodeId, role: &str) -> String {
    nama_dalam(doc, id, role, false, &mut Vec::new())
}

/// `sebagai_label` berarti simpul ini sedang dipakai sebagai sumber nama
/// (`aria-labelledby`), jadi isi teksnya dihitung walaupun perannya tidak ada
/// di [`NAMA_DARI_ISI`]. Tanpa itu, `<span id="lbl">Nama</span>` akan
/// menyumbang string kosong karena `span` berperan `generic`.
///
/// `dikunjungi` adalah penjaga siklus: `a` yang menunjuk `b` yang menunjuk `a`
/// akan berputar selamanya kalau tidak dicatat. Yang dikembalikan untuk siklus
/// adalah string kosong, bukan galat, karena accname yang melingkar memang
/// tidak punya nama yang bisa dibacakan.
fn nama_dalam(
    doc: &Document,
    id: NodeId,
    role: &str,
    sebagai_label: bool,
    dikunjungi: &mut Vec<NodeId>,
) -> String {
    let Some(node) = doc.node(id) else {
        return String::new();
    };
    if dikunjungi.contains(&id) {
        return String::new();
    }
    dikunjungi.push(id);

    if let Some(daftar) = node.attr("aria-labelledby") {
        let gabung = daftar
            .split_whitespace()
            .filter_map(|rujukan| doc.cari_id(rujukan))
            .map(|target| {
                let peran_target = peran(doc, target);
                nama_dalam(doc, target, &peran_target, true, dikunjungi)
            })
            .filter(|teks| !teks.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !gabung.is_empty() {
            return gabung;
        }
    }

    for attr in ["aria-label", "alt", "placeholder", "title"] {
        if let Some(v) = node.attr(attr) {
            let rapi = v.trim();
            if !rapi.is_empty() {
                return rapi.to_string();
            }
        }
    }
    // `value` hanya menamai kontrol form, dan hanya pada `<input>`.
    // `<option value="x">Label</option>` dibacakan sebagai "Label", bukan
    // "x": isi elemennya yang dilihat pengguna, sedangkan `value` adalah
    // nilai yang dikirim ke server. Memakai `value` di sana akan menamai
    // pilihan dengan teks yang tidak pernah tampil di layar.
    if node.tag() == Some("input")
        && let Some(v) = node.attr("value")
    {
        let rapi = v.trim();
        if !rapi.is_empty() {
            return rapi.to_string();
        }
    }
    if sebagai_label || NAMA_DARI_ISI.contains(&role) {
        return doc.teks(id).trim().to_string();
    }
    String::new()
}

/// Nilai boolean dari atribut yang boleh ditulis `true`/`false` (ARIA), atau
/// hadir-tanpa-nilai (HTML, mis. `disabled`).
fn boolean_attr(doc: &Document, id: NodeId, nama_attr: &str) -> Option<bool> {
    let node = doc.node(id)?;
    let v = node.attr(nama_attr)?;
    let rapi = v.trim();
    if rapi.is_empty() {
        return Some(true);
    }
    if rapi.eq_ignore_ascii_case("true") {
        return Some(true);
    }
    if rapi.eq_ignore_ascii_case("false") {
        return Some(false);
    }
    None
}

/// Tag yang bisa menerima fokus tanpa `tabindex`.
const FOKUSABLE: &[&str] = &[
    "a", "button", "input", "select", "textarea", "summary", "iframe", "object", "audio", "video",
];

/// Apakah elemen ini bisa menerima fokus.
///
/// `tabindex="-1"` tetap fokusabel secara program, hanya tidak lewat Tab, jadi
/// keberadaan atributnya sudah cukup. `<a>` tanpa `href` tidak fokusabel, dan
/// elemen `disabled` tidak fokusabel sekalipun tag-nya ada di daftar.
fn fokusabel(doc: &Document, id: NodeId) -> bool {
    let Some(node) = doc.node(id) else {
        return false;
    };
    if boolean_attr(doc, id, "disabled").unwrap_or(false)
        || boolean_attr(doc, id, "aria-disabled").unwrap_or(false)
    {
        return false;
    }
    if node.attr("tabindex").is_some() {
        return true;
    }
    let Some(tag) = node.tag() else {
        return false;
    };
    if tag == "a" {
        return node.attr("href").is_some();
    }
    FOKUSABLE.contains(&tag)
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
        // Status diambil dari atribut, bukan dari state internal: mesin ini
        // belum menyimpan state fokus atau buka-tutup, jadi yang bisa
        // dipertanggungjawabkan hanya apa yang tertulis di dokumen. Menebak
        // sisanya akan mengarang keadaan yang tidak pernah terjadi.
        let disabled = boolean_attr(doc, id, "disabled").unwrap_or(false)
            || boolean_attr(doc, id, "aria-disabled").unwrap_or(false);
        let selected = boolean_attr(doc, id, "aria-selected")
            .or_else(|| boolean_attr(doc, id, "selected"))
            .filter(|_| {
                matches!(
                    role.as_str(),
                    "option" | "tab" | "row" | "gridcell" | "treeitem"
                )
            });
        let expanded = boolean_attr(doc, id, "aria-expanded").filter(|_| {
            matches!(
                role.as_str(),
                "combobox" | "button" | "treeitem" | "row" | "group" | "generic"
            )
        });
        let checked = boolean_attr(doc, id, "aria-checked")
            .or_else(|| boolean_attr(doc, id, "aria-pressed"))
            .or_else(|| {
                matches!(role.as_str(), "checkbox" | "radio")
                    .then(|| boolean_attr(doc, id, "checked").unwrap_or(false))
            });
        out.push(AxNode {
            id: nomor.clone(),
            role,
            name: label,
            ignored,
            backend: Some(id),
            parent: induk.clone(),
            children: Vec::new(),
            focusable: !ignored && fokusabel(doc, id),
            focused: false,
            selected,
            expanded,
            disabled,
            checked,
        });
        // Anak komposit, sama seperti yang ditata dan dilukis: pohon terang,
        // isi shadow root, lalu isi dokumen `<iframe>` kalau ada.
        //
        // Node dokumen milik bingkai sengaja tidak dijadikan simpul AX
        // sendiri. Pembaca pohon ini menelusuri satu pohon; menyisipkan
        // dokumen kedua di tengahnya hanya menambah simpul tanpa peran yang
        // tidak bisa dipetakan ke apa pun di DOM.
        let mut anak: Vec<NodeId> = doc.anak_tata(id);
        if let Some(dok_anak) = doc.bingkai(id) {
            anak.extend(doc.anak_tata(dok_anak));
        }
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

/// Satu entri `properties` CDP: `{"name": ..., "value": {"type": ..., "value": ...}}`.
fn properti(nama: &str, nilai: bool) -> Value {
    json!({
        "name": nama,
        "value": { "type": "booleanOrUndefined", "value": nilai },
    })
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
            // Status dikirim sebagai `properties`, sama seperti Chrome. Yang
            // `None` sengaja tidak dikirim: klien membedakan "tidak berlaku"
            // dari "false", dan mengirim `false` untuk elemen yang tidak punya
            // konsep itu akan membuat agen membacanya sebagai keadaan nyata.
            let mut props: Vec<Value> = Vec::new();
            if n.focusable {
                props.push(properti("focusable", true));
            }
            if n.focused {
                props.push(properti("focused", true));
            }
            if n.disabled {
                props.push(properti("disabled", true));
            }
            if let Some(v) = n.selected {
                props.push(properti("selected", v));
            }
            if let Some(v) = n.expanded {
                props.push(properti("expanded", v));
            }
            if let Some(v) = n.checked {
                props.push(properti("checked", v));
            }
            if !props.is_empty() {
                obj["properties"] = Value::Array(props);
            }
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

    #[test]
    fn aria_labelledby_menggabung_beberapa_id() {
        let doc = Document::parse_html(
            "<body><span id=\"depan\">Halo</span><span id=\"belakang\">Dunia</span>\
             <button aria-labelledby=\"depan belakang\">X</button></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        assert_eq!(
            cari(&simpul, "button", "Halo Dunia").name,
            "Halo Dunia",
            "dua id digabung dengan spasi, berurutan sesuai atributnya"
        );
    }

    #[test]
    fn aria_labelledby_menang_atas_isi_elemen() {
        // Elemen yang menunjuk sumber nama lain harus memakai sumber itu,
        // bukan teksnya sendiri; kalau tidak, tombol berikon akan terbaca
        // sebagai "X" dan bukan sebagai maksud sebenarnya.
        let doc = Document::parse_html(
            "<body><span id=\"lbl\">Tutup dialog</span>\
             <button aria-labelledby=\"lbl\">X</button></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        assert_eq!(cari(&simpul, "button", "Tutup dialog").name, "Tutup dialog");
        assert!(
            simpul.iter().all(|n| n.name != "X"),
            "teks tombol tidak boleh dipakai saat aria-labelledby ada"
        );
    }

    #[test]
    fn aria_labelledby_berantai_dan_siklus_berhenti() {
        // Berantai: label menunjuk label lain. Melingkar: dua elemen saling
        // menunjuk. Yang kedua tidak boleh berputar selamanya; hasilnya nama
        // kosong, dan pohonnya tetap selesai dibangun.
        let doc = Document::parse_html(
            "<body><span id=\"a\" aria-labelledby=\"b\">dari-a</span>\
             <span id=\"b\">dari-b</span>\
             <span id=\"p\" aria-labelledby=\"q\">p</span>\
             <span id=\"q\" aria-labelledby=\"p\">q</span>\
             <button aria-labelledby=\"a\">X</button></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        assert_eq!(
            cari(&simpul, "button", "dari-b").name,
            "dari-b",
            "rantai a->b harus sampai ke ujungnya"
        );
    }

    #[test]
    fn siklus_labelledby_tidak_menggantung() {
        let doc = Document::parse_html(
            "<body><span id=\"p\" aria-labelledby=\"q\">p</span>\
             <span id=\"q\" aria-labelledby=\"p\">q</span></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        // Inti tes ini bukan nilai namanya, tapi bahwa panggilan di atas
        // KEMBALI. Tanpa penjaga siklus, pohon(&doc) tidak pernah selesai.
        assert!(!simpul.is_empty(), "pohon tetap terbangun");
    }

    #[test]
    fn status_focusable_selected_expanded_terbaca() {
        let doc = Document::parse_html(
            "<body><input type=\"checkbox\" checked>\
             <button aria-expanded=\"true\">Menu</button>\
             <select><option selected>satu</option><option>dua</option></select>\
             <a href=\"/x\">tautan</a><a>tanpa-href</a>\
             <button disabled>mati</button></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);

        let kotak = cari(&simpul, "checkbox", "");
        assert_eq!(kotak.checked, Some(true), "checked pada checkbox");
        assert!(kotak.focusable, "checkbox fokusabel");

        let menu = cari(&simpul, "button", "Menu");
        assert_eq!(menu.expanded, Some(true), "aria-expanded terbaca");

        let satu = cari(&simpul, "option", "satu");
        assert_eq!(satu.selected, Some(true), "<option selected> terbaca");

        assert!(
            cari(&simpul, "link", "tautan").focusable,
            "<a href> fokusabel"
        );
        assert!(
            !cari(&simpul, "link", "tanpa-href").focusable,
            "<a> tanpa href tidak fokusabel"
        );

        let mati = cari(&simpul, "button", "mati");
        assert!(mati.disabled, "disabled terbaca");
        assert!(!mati.focusable, "elemen disabled tidak fokusabel");
    }

    #[test]
    fn properties_hanya_memuat_status_yang_berlaku() {
        let doc = Document::parse_html(
            "<body><button aria-expanded=\"false\">Tutup</button><p>teks</p></body>",
        )
        .unwrap();
        let isi = json_pohon(&doc);
        let simpul = isi["nodes"].as_array().expect("nodes array");

        let tombol = simpul
            .iter()
            .find(|n| n["role"]["value"] == "button")
            .expect("tombol ada");
        let props = tombol["properties"]
            .as_array()
            .expect("tombol punya properties");
        let nama_props: Vec<&str> = props.iter().filter_map(|p| p["name"].as_str()).collect();
        assert!(nama_props.contains(&"focusable"), "{nama_props:?}");
        assert!(nama_props.contains(&"expanded"), "{nama_props:?}");
        assert!(
            !nama_props.contains(&"selected"),
            "tombol tidak punya konsep selected: {nama_props:?}"
        );

        let ekspansi = props
            .iter()
            .find(|p| p["name"] == "expanded")
            .expect("ada properti expanded");
        assert_eq!(
            ekspansi["value"]["value"],
            json!(false),
            "false tetap dikirim"
        );
        assert_eq!(ekspansi["value"]["type"], "booleanOrUndefined");

        let paragraf = simpul
            .iter()
            .find(|n| n["role"]["value"] == "paragraph")
            .expect("paragraf ada");
        assert!(
            paragraf.get("properties").is_none(),
            "simpul tanpa status tidak punya array properties kosong"
        );
    }

    #[test]
    fn nama_option_dari_isi_bukan_dari_value() {
        // `value` adalah nilai yang dikirim ke server, bukan yang dibaca
        // pengguna. Memakainya sebagai nama akan membuat daftar pilihan
        // terbaca sebagai kode internal yang tidak pernah tampil di layar.
        let doc = Document::parse_html(
            "<body><select><option value=\"kode-1\">Pilihan satu</option></select></body>",
        )
        .unwrap();
        let simpul = pohon(&doc);
        assert_eq!(cari(&simpul, "option", "Pilihan satu").name, "Pilihan satu");
        assert!(
            simpul.iter().all(|n| n.name != "kode-1"),
            "nilai kirim tidak boleh jadi nama"
        );
    }
}
