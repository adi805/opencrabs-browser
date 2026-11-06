//! Memilih mesin untuk satu permintaan, tanpa fallback senyap.
//!
//! Urutan evaluasi mengikuti dokumen `02-ENGINE-SESI` §4, ditambah satu aturan
//! yang datang dari berkas config: `[policy.rules]` memetakan host ke mesin.
//! Aturan config diletakkan setelah permintaan eksplisit dan hint profil,
//! karena keduanya milik satu sesi sedangkan config milik seluruh daemon.
//!
//! **Aturan yang dilarang: fallback senyap.** Kalau mesin pilihan tidak mampu
//! memenuhi kebutuhan permintaan, jawabannya galat yang menyebut kemampuan yang
//! kurang. Diam-diam pindah ke mesin lain membuat bug rendering tidak pernah
//! ketahuan, dan itu lebih mahal daripada satu permintaan yang gagal jelas.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Mesin yang dikenal. Nama string-nya sama dengan nama bagian di config
/// (`[engines.chrome_shell]`) supaya pesan galat bisa dicari di berkas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// `chrome-headless-shell`: paling terbukti, tanpa layar.
    Shell,
    /// Chromium penuh: fingerprint konsisten untuk situs bertantangan berat.
    Full,
    /// Mesin pihak ketiga, hanya dipakai kalau uji kapabilitasnya lulus.
    Oxi,
}

impl EngineKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "chrome-shell",
            Self::Full => "chrome-full",
            Self::Oxi => "oxi",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "chrome-shell" | "chrome_shell" | "shell" => Some(Self::Shell),
            "chrome-full" | "chrome_full" | "full" => Some(Self::Full),
            "oxi" => Some(Self::Oxi),
            _ => None,
        }
    }
}

/// Kemampuan satu mesin, diukur bukan diasumsikan.
///
/// `layout` berarti mesin bisa menghitung posisi elemen dan mencetak PDF.
/// `script` berarti JavaScript berjalan. Mesin teks saja tidak punya keduanya.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub layout: bool,
    pub script: bool,
    pub cookies: bool,
    pub input: bool,
}

impl Capabilities {
    /// Kemampuan yang dibutuhkan tapi tidak dimiliki. Kosong berarti cukup.
    #[must_use]
    pub fn missing(self, needed: Self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if needed.layout && !self.layout {
            out.push("layout");
        }
        if needed.script && !self.script {
            out.push("script");
        }
        if needed.cookies && !self.cookies {
            out.push("cookies");
        }
        if needed.input && !self.input {
            out.push("input");
        }
        out
    }

    #[must_use]
    pub const fn satisfies(self, needed: Self) -> bool {
        (!needed.layout || self.layout)
            && (!needed.script || self.script)
            && (!needed.cookies || self.cookies)
            && (!needed.input || self.input)
    }
}

/// Mesin yang tersedia di mesin ini, beserta kemampuannya.
///
/// `None` berarti mesinnya tidak terpasang, dan itu berbeda dari "terpasang
/// tapi tidak mampu". Keduanya butuh pesan galat yang berbeda.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub shell: Option<Capabilities>,
    pub full: Option<Capabilities>,
    pub oxi: Option<Capabilities>,
}

impl Catalog {
    #[must_use]
    pub fn capabilities(&self, kind: EngineKind) -> Option<Capabilities> {
        match kind {
            EngineKind::Shell => self.shell,
            EngineKind::Full => self.full,
            EngineKind::Oxi => self.oxi,
        }
    }

    /// Katalog bawaan untuk mesin ini: shell selalu ada kalau binernya
    /// ditemukan, full dan oxi hanya kalau dipasang.
    #[must_use]
    pub fn with_shell(shell: Capabilities) -> Self {
        Self {
            shell: Some(shell),
            full: None,
            oxi: None,
        }
    }
}

impl Catalog {
    /// Katalog dari daftar mesin yang **benar-benar terpasang** di host ini.
    ///
    /// Kemampuan tiap jenis diambil dari konstanta `TERUKUR_*` di bawah, bukan
    /// dari harapan. Jenis mesin yang tidak ada di daftar berarti tidak
    /// terpasang (`None`), dan itu yang membuat aturan config yang menunjuk
    /// mesin hilang bisa ditolak saat start, bukan saat klien pertama datang.
    ///
    /// Mesin yang **terpasang tapi belum diukur** tetap masuk katalog dengan
    /// `Capabilities::default()` (semua `false`). Itu bukan kelalaian: mengisi
    /// kemampuan tanpa bukti membuat permintaan yang tidak mungkin dipenuhi
    /// diteruskan sampai gagal di tengah jalan, dan kegagalan seperti itu jauh
    /// lebih mahal daripada satu penolakan yang jelas di depan.
    #[must_use]
    pub fn dari_mesin<'a>(ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut katalog = Self::default();
        for id in ids {
            let Some(kind) = EngineKind::parse(id) else {
                continue;
            };
            let caps = match kind {
                EngineKind::Shell => TERUKUR_SHELL,
                EngineKind::Full => TERUKUR_FULL,
                EngineKind::Oxi => TERUKUR_OXI,
            };
            match kind {
                EngineKind::Shell => katalog.shell = Some(caps),
                EngineKind::Full => katalog.full = Some(caps),
                EngineKind::Oxi => katalog.oxi = Some(caps),
            }
        }
        katalog
    }
}

/// Kemampuan `chrome-headless-shell` yang **sudah diukur**.
///
/// Bukti: selftest milik `browserd` membuka tab sungguhan, menjalankan skrip,
/// membaca cookie, dan mengirim input ke mesin ini; keempatnya lolos sejak
/// milestone 1. Kalau salah satu berhenti lolos, ubah konstanta ini, jangan
/// ubah test-nya.
pub const TERUKUR_SHELL: Capabilities = Capabilities {
    layout: true,
    script: true,
    cookies: true,
    input: true,
};

/// Kemampuan Chromium penuh. **Belum pernah diukur di repo ini**, dan adapter
/// `chrome-full` belum ada. Semua `false` sampai ada bukti, bukan karena
/// diragukan.
pub const TERUKUR_FULL: Capabilities = Capabilities {
    layout: false,
    script: false,
    cookies: false,
    input: false,
};

/// Kemampuan `oxibrowser` yang **sudah diukur** oleh
/// `crates/browserd/tests/oxi_probe.rs`.
///
/// Nilainya diisi dari keluaran probe, bukan dari README proyek itu: Oxi
/// mengklaim CDP kompatibel, dan justru klaim itulah yang harus dibuktikan.
/// Selama putaran probe belum tercatat di `docs/prd/BUKTI-OXI *.md`, mesin oxi
/// tetap `false` di sini sehingga permintaan yang butuh kemampuan itu ditolak
/// di depan alih-alih gagal di tengah jalan.
pub const TERUKUR_OXI: Capabilities = Capabilities {
    layout: false,
    script: false,
    cookies: false,
    input: false,
};

/// Satu permintaan yang butuh mesin.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// `openCrabsOptions.engine`: permintaan eksplisit pemanggil.
    pub requested: Option<EngineKind>,
    /// `engine_hint` milik profil yang dipakai sesi ini.
    pub profile_hint: Option<EngineKind>,
    pub host: String,
    /// Kebutuhan nyata task. Dipakai untuk menolak mesin yang tidak mampu.
    pub needs: Capabilities,
    pub wants_pdf: bool,
    pub wants_screenshot: bool,
}

/// Kenapa sebuah permintaan tidak bisa dipenuhi.
///
/// Semua varian menyebut mesin dan alasannya. Galat yang cuma bilang "gagal"
/// memaksa orang membaca kode untuk tahu apa yang harus dipasang.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectError {
    UnknownEngineName(String),
    RequestedNotInstalled(EngineKind),
    RequestedIncapable {
        kind: EngineKind,
        missing: Vec<&'static str>,
    },
    ProfileHintNotInstalled(EngineKind),
    ProfileHintIncapable {
        kind: EngineKind,
        missing: Vec<&'static str>,
    },
    StealthHostNeedsFull {
        host: String,
    },
    /// Mesin yang dipilih aturan config/stealth tidak terpasang. Varian
    /// terpisah dari [`Self::ProfileHintNotInstalled`] karena yang harus
    /// diperbaiki berbeda: config, bukan profil.
    NotInstalled {
        kind: EngineKind,
        rule: &'static str,
    },
    NotCapable {
        kind: EngineKind,
        missing: Vec<&'static str>,
        rule: &'static str,
    },
    NoEngineWithCapabilities {
        missing: Vec<&'static str>,
    },
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEngineName(name) => {
                write!(f, "nama mesin '{name}' tidak dikenal")
            }
            Self::RequestedNotInstalled(kind) => {
                write!(
                    f,
                    "mesin {} diminta permintaan tapi tidak terpasang",
                    kind.as_str()
                )
            }
            Self::RequestedIncapable { kind, missing } => write!(
                f,
                "mesin {} diminta permintaan tapi tidak punya: {}",
                kind.as_str(),
                missing.join(", ")
            ),
            Self::ProfileHintNotInstalled(kind) => write!(
                f,
                "profil menunjuk {} tapi mesin itu tidak terpasang",
                kind.as_str()
            ),
            Self::ProfileHintIncapable { kind, missing } => write!(
                f,
                "profil menunjuk {} tapi mesin itu tidak punya: {}",
                kind.as_str(),
                missing.join(", ")
            ),
            Self::StealthHostNeedsFull { host } => write!(
                f,
                "host {host} ada di daftar stealth tapi chrome-full tidak terpasang \
                 (sengaja tidak jatuh ke chrome-shell: fingerprint-nya berbeda)"
            ),
            Self::NotInstalled { kind, rule } => write!(
                f,
                "aturan {rule} memilih {} tapi mesin itu tidak terpasang",
                kind.as_str()
            ),
            Self::NotCapable {
                kind,
                missing,
                rule,
            } => write!(
                f,
                "aturan {rule} memilih {} tapi mesin itu tidak punya: {}",
                kind.as_str(),
                missing.join(", ")
            ),
            Self::NoEngineWithCapabilities { missing } => write!(
                f,
                "tidak ada mesin terpasang yang punya: {}",
                missing.join(", ")
            ),
        }
    }
}

impl std::error::Error for SelectError {}

/// Bagian `[policy]` di `browser-daemon.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PolicySection {
    /// Mesin bawaan saat tidak ada aturan lain yang cocok.
    ///
    /// `None` berarti operator **tidak menulis** `[policy] default`, dan itu
    /// bukan hal yang sama dengan menulis `chrome-shell`. Pemanggil yang punya
    /// mesin bawaan sendiri (daemon: `[daemon] engine_default`) mengisinya dari
    /// sana; di sini nilainya dibiarkan kosong supaya tidak ada nama mesin yang
    /// dikarang di dalam kode.
    pub default: Option<String>,
    /// Host yang wajib memakai `chrome-full` karena tantangannya berat.
    pub stealth_hosts: Vec<String>,
    /// Pemetaan host ke nama mesin, mis. `"shop.example" = "chrome-full"`.
    pub rules: BTreeMap<String, String>,
}

impl PolicySection {
    /// Nama mesin untuk sebuah host kalau ada aturan eksplisit.
    #[must_use]
    pub fn rule_for(&self, host: &str) -> Option<&str> {
        self.rules.get(host).map(String::as_str).or_else(|| {
            // Aturan domain induk berlaku untuk subdomain: menulis
            // "cloudflare.com" harus menutup "challenges.cloudflare.com",
            // karena situs tantangan hampir selalu ada di subdomain.
            self.rules.iter().find_map(|(pattern, engine)| {
                host_matches(pattern, host).then_some(engine.as_str())
            })
        })
    }

    #[must_use]
    pub fn is_stealth_host(&self, host: &str) -> bool {
        self.stealth_hosts
            .iter()
            .any(|pattern| host_matches(pattern, host))
    }
}

impl PolicySection {
    /// Periksa seluruh isi `[policy]` terhadap mesin yang terpasang.
    ///
    /// Dijalankan **saat start**, bukan saat permintaan pertama: aturan yang
    /// menunjuk mesin yang tidak ada adalah salah tulis config, dan salah tulis
    /// config harus terlihat sebagai daemon yang menolak start dengan pesan
    /// yang menyebut nama mesinnya. Kalau dibiarkan sampai permintaan datang,
    /// satu host akan gagal jauh setelah operator lupa apa yang dia ubah.
    ///
    /// Semua pelanggaran dikumpulkan, bukan yang pertama saja: config dengan
    /// tiga aturan salah butuh satu perbaikan, bukan tiga kali start.
    ///
    /// # Errors
    ///
    /// Mengembalikan daftar pelanggaran; kosong berarti aturan sah.
    pub fn validasi(&self, catalog: &Catalog) -> Result<(), Vec<SelectError>> {
        let mut salah = Vec::new();

        // Tanpa `[policy] default` tidak ada yang perlu diperiksa: mesin
        // bawaan diambil dari `[daemon] engine_default` oleh pemanggil.
        if let Some(bawaan) = &self.default {
            match EngineKind::parse(bawaan) {
                None => salah.push(SelectError::UnknownEngineName(bawaan.clone())),
                Some(kind) if catalog.capabilities(kind).is_none() => {
                    salah.push(SelectError::NotInstalled {
                        kind,
                        rule: RULE_DEFAULT,
                    });
                }
                Some(_) => {}
            }
        }

        for (host, nama) in &self.rules {
            match EngineKind::parse(nama) {
                None => salah.push(SelectError::UnknownEngineName(nama.clone())),
                Some(kind) if catalog.capabilities(kind).is_none() => {
                    salah.push(SelectError::NotInstalled {
                        kind,
                        rule: RULE_CONFIG,
                    });
                }
                Some(_) => {}
            }
            // Host kosong tidak akan pernah cocok dengan apa pun, jadi aturannya
            // mati tanpa suara. Itu salah tulis juga, bukan sekadar tidak rapi.
            if host.trim().is_empty() {
                salah.push(SelectError::UnknownEngineName(nama.clone()));
            }
        }

        for host in &self.stealth_hosts {
            if catalog.full.is_none() {
                salah.push(SelectError::StealthHostNeedsFull { host: host.clone() });
            }
        }

        if salah.is_empty() { Ok(()) } else { Err(salah) }
    }
}

/// `host` cocok dengan `pattern` kalau sama persis atau subdomain darinya.
///
/// Pencocokan dilakukan pada label domain, bukan potongan string: `evilcloudflare.com`
/// tidak boleh cocok dengan `cloudflare.com`.
#[must_use]
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().trim_start_matches("*.").to_ascii_lowercase();
    let host = host.trim().to_ascii_lowercase();
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    host == pattern
        || host
            .strip_suffix(&pattern)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// Bagian `[captcha]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CaptchaSection {
    pub handoff_per_host_hour: u32,
    pub handoff_total_day: u32,
    pub cooldown_secs: u64,
}

impl Default for CaptchaSection {
    fn default() -> Self {
        Self {
            handoff_per_host_hour: 5,
            handoff_total_day: 20,
            cooldown_secs: 600,
        }
    }
}

/// Mesin yang dipilih, plus aturan mana yang menang.
///
/// `rule` ikut dikembalikan karena "kenapa mesin ini" adalah pertanyaan pertama
/// saat ada bug rendering, dan menebaknya dari kode mahal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub kind: EngineKind,
    pub rule: &'static str,
}

const RULE_REQUESTED: &str = "requested";
const RULE_PROFILE: &str = "profile_hint";
const RULE_CONFIG: &str = "host_rule";
const RULE_STEALTH: &str = "stealth_host";
const RULE_PDF: &str = "print_to_pdf";
const RULE_LAYOUT: &str = "layout_needed";
const RULE_CHEAPEST: &str = "cheapest_script";
const RULE_DEFAULT: &str = "default";

/// Urutan evaluasi berhenti di aturan pertama yang cocok.
pub fn select(
    request: &Request,
    config: &PolicySection,
    catalog: &Catalog,
) -> Result<Decision, SelectError> {
    // 1. Permintaan eksplisit pemanggil. Kalau tidak terpasang atau tidak
    //    mampu, itu galat: memenuhi permintaan dengan mesin lain adalah
    //    kebohongan tentang apa yang benar-benar dijalankan.
    if let Some(kind) = request.requested {
        return require(
            kind,
            catalog,
            request.needs,
            RULE_REQUESTED,
            ErrorKind::Requested,
        );
    }

    // 2. Hint profil. Alasannya konsistensi fingerprint, jadi menggantinya
    //    diam-diam menghapus gunanya hint itu sendiri.
    if let Some(kind) = request.profile_hint {
        return require(
            kind,
            catalog,
            request.needs,
            RULE_PROFILE,
            ErrorKind::ProfileHint,
        );
    }

    // 3. Aturan host di config.
    if let Some(name) = config.rule_for(&request.host) {
        let kind = EngineKind::parse(name)
            .ok_or_else(|| SelectError::UnknownEngineName(name.to_owned()))?;
        return require(
            kind,
            catalog,
            request.needs,
            RULE_CONFIG,
            ErrorKind::HostRule,
        );
    }

    // 4. Host bertantangan berat: wajib chrome-full.
    if config.is_stealth_host(&request.host) {
        if catalog.full.is_none() {
            return Err(SelectError::StealthHostNeedsFull {
                host: request.host.clone(),
            });
        }
        return require(
            EngineKind::Full,
            catalog,
            request.needs,
            RULE_STEALTH,
            ErrorKind::HostRule,
        );
    }

    // 5. PDF butuh layout.
    if request.wants_pdf {
        return choose_with_layout(catalog, request.needs, RULE_PDF);
    }

    // 6. Screenshot juga butuh layout. Oxi dipakai hanya kalau shell tidak ada
    //    dan oxi benar-benar punya layout (keputusan ada di katalog, bukan di
    //    sini: yang tahu kemampuan oxi adalah hasil uji kapabilitasnya).
    if request.wants_screenshot {
        return choose_with_layout(catalog, request.needs, RULE_LAYOUT);
    }

    // 7. Teks saja: mesin termurah yang bisa menjalankan JavaScript.
    if !request.needs.layout
        && let Some(kind) = cheapest_script(catalog, request.needs)
    {
        return Ok(Decision {
            kind,
            rule: RULE_CHEAPEST,
        });
    }

    // 8. Bawaan dari config, lalu shell. Shell adalah nama terakhir yang
    //    dipakai, bukan nama yang dikarang: tanpanya, config yang hanya berisi
    //    satu mesin lain tidak punya jalan sama sekali.
    let nama = config
        .default
        .clone()
        .unwrap_or_else(|| EngineKind::Shell.as_str().to_owned());
    let fallback =
        EngineKind::parse(&nama).ok_or_else(|| SelectError::UnknownEngineName(nama.clone()))?;
    require(
        fallback,
        catalog,
        request.needs,
        RULE_DEFAULT,
        ErrorKind::Default,
    )
}

/// Pemilihan mesin saat yang diketahui cuma host, bukan kebutuhan task.
///
/// Dipakai jalur protokol (`Target.createTarget`): di sana hanya URL yang
/// tersedia, tidak ada daftar kemampuan yang diminta. Karena itu fungsi ini
/// **hanya** memutuskan kalau ada aturan host yang berlaku. Tanpa aturan,
/// `Ok(None)` berarti pemanggil memakai mesin bawaannya, bukan mesin termurah:
/// menjalankan seluruh trafik ke mesin termurah cuma karena `[policy.rules]`
/// tidak menyebut host-nya adalah keputusan yang tidak pernah diambil siapa pun.
///
/// `host` wajib tanpa skema dan tanpa port; pemanggil yang mengekstraknya dari
/// URL bertanggung jawab membuang keduanya. `"toko.test:8080"` tidak cocok
/// dengan aturan `"toko.test"`, dan itu disengaja: menebak di sini berarti dua
/// bentuk penulisan host memberi dua keputusan berbeda tanpa jejak.
///
/// # Errors
///
/// Meneruskan galat [`select`] apa adanya: aturan yang menunjuk mesin tidak
/// terpasang atau mesin yang tidak mampu adalah penolakan, bukan alasan
/// memakai mesin bawaan.
pub fn select_untuk_host(
    host: &str,
    config: &PolicySection,
    catalog: &Catalog,
) -> Result<Option<Decision>, SelectError> {
    if config.rule_for(host).is_none() && !config.is_stealth_host(host) {
        return Ok(None);
    }
    let request = Request {
        host: host.to_owned(),
        ..Request::default()
    };
    select(&request, config, catalog).map(Some)
}

fn choose_with_layout(
    catalog: &Catalog,
    needs: Capabilities,
    rule: &'static str,
) -> Result<Decision, SelectError> {
    for kind in [EngineKind::Shell, EngineKind::Oxi, EngineKind::Full] {
        if let Some(caps) = catalog.capabilities(kind)
            && caps.satisfies(needs)
            && caps.layout
        {
            return Ok(Decision { kind, rule });
        }
    }

    // Pesan galat menyebut kebutuhan permintaan, bukan sekadar "tidak ada
    // mesin": yang bisa dipasang orang berikutnya adalah kemampuan yang kurang.
    let mut missing = needs.missing(Capabilities::default());
    if missing.is_empty() {
        missing.push("layout");
    }
    Err(SelectError::NoEngineWithCapabilities { missing })
}

fn cheapest_script(catalog: &Catalog, needs: Capabilities) -> Option<EngineKind> {
    [EngineKind::Oxi, EngineKind::Shell, EngineKind::Full]
        .into_iter()
        .find(|kind| {
            catalog
                .capabilities(*kind)
                .is_some_and(|caps| caps.script && caps.satisfies(needs))
        })
}

/// Sumber aturan, dipakai hanya untuk memilih varian galat yang tepat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    /// pemanggil menyebut mesinnya sendiri
    Requested,
    /// hint profil
    ProfileHint,
    /// aturan host atau daftar stealth: bukan permintaan satu sesi
    HostRule,
    /// mesin bawaan dari config
    Default,
}

/// Memastikan mesin yang dipilih aturan ini terpasang dan mampu.
///
/// `rule` dibawa terpisah dari `ErrorKind` karena aturan yang menang dan sumber
/// galatnya tidak selalu sama: aturan config dan daftar stealth sama-sama
/// "aturan host", tapi label keputusannya berbeda dan label itulah yang dibaca
/// orang saat mencari kenapa mesin ini yang dipakai.
fn require(
    kind: EngineKind,
    catalog: &Catalog,
    needs: Capabilities,
    rule: &'static str,
    sumber: ErrorKind,
) -> Result<Decision, SelectError> {
    let Some(caps) = catalog.capabilities(kind) else {
        return Err(match sumber {
            ErrorKind::Requested => SelectError::RequestedNotInstalled(kind),
            ErrorKind::ProfileHint => SelectError::ProfileHintNotInstalled(kind),
            ErrorKind::HostRule | ErrorKind::Default => SelectError::NotInstalled { kind, rule },
        });
    };
    let missing = caps.missing(needs);
    if !missing.is_empty() {
        return Err(match sumber {
            ErrorKind::Requested => SelectError::RequestedIncapable { kind, missing },
            ErrorKind::ProfileHint => SelectError::ProfileHintIncapable { kind, missing },
            ErrorKind::HostRule | ErrorKind::Default => SelectError::NotCapable {
                kind,
                missing,
                rule,
            },
        });
    }
    Ok(Decision { kind, rule })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semua() -> Catalog {
        Catalog {
            shell: Some(Capabilities {
                layout: true,
                script: true,
                cookies: true,
                input: true,
            }),
            full: Some(Capabilities {
                layout: true,
                script: true,
                cookies: true,
                input: true,
            }),
            oxi: Some(Capabilities {
                layout: false,
                script: true,
                cookies: true,
                input: true,
            }),
        }
    }

    fn permintaan(host: &str) -> Request {
        Request {
            host: host.to_owned(),
            ..Request::default()
        }
    }

    #[test]
    fn permintaan_eksplisit_menang_atas_semua_aturan() {
        let request = Request {
            requested: Some(EngineKind::Full),
            profile_hint: Some(EngineKind::Shell),
            wants_pdf: true,
            ..permintaan("contoh.test")
        };
        let decision =
            select(&request, &PolicySection::default(), &semua()).expect("harus dipilih");
        assert_eq!(decision.kind, EngineKind::Full);
        assert_eq!(decision.rule, RULE_REQUESTED);
    }

    #[test]
    fn permintaan_eksplisit_yang_tidak_terpasang_ditolak_bukan_diganti() {
        let request = Request {
            requested: Some(EngineKind::Oxi),
            ..permintaan("contoh.test")
        };
        let katalog = Catalog::with_shell(Capabilities {
            layout: true,
            script: true,
            cookies: true,
            input: true,
        });
        let err = select(&request, &PolicySection::default(), &katalog).expect_err("harus ditolak");
        assert_eq!(err, SelectError::RequestedNotInstalled(EngineKind::Oxi));
        assert!(err.to_string().contains("oxi"), "pesan: {err}");
    }

    #[test]
    fn permintaan_eksplisit_yang_tidak_mampu_menyebut_kemampuannya() {
        let request = Request {
            requested: Some(EngineKind::Oxi),
            needs: Capabilities {
                layout: true,
                ..Capabilities::default()
            },
            ..permintaan("contoh.test")
        };
        let err = select(&request, &PolicySection::default(), &semua())
            .expect_err("oxi tanpa layout harus ditolak");
        assert_eq!(
            err,
            SelectError::RequestedIncapable {
                kind: EngineKind::Oxi,
                missing: vec!["layout"]
            }
        );
    }

    #[test]
    fn hint_profil_dipakai_untuk_konsistensi_fingerprint() {
        let request = Request {
            profile_hint: Some(EngineKind::Shell),
            host: "toko.test".to_owned(),
            ..Request::default()
        };
        let config = PolicySection {
            rules: BTreeMap::from([("toko.test".to_owned(), "chrome-full".to_owned())]),
            ..PolicySection::default()
        };
        let decision = select(&request, &config, &semua()).expect("harus dipilih");
        assert_eq!(decision.kind, EngineKind::Shell);
        assert_eq!(decision.rule, RULE_PROFILE);
    }

    #[test]
    fn aturan_config_host_dan_subdomain() {
        let config = PolicySection {
            rules: BTreeMap::from([("toko.test".to_owned(), "chrome-full".to_owned())]),
            ..PolicySection::default()
        };
        let decision = select(&permintaan("toko.test"), &config, &semua()).expect("harus dipilih");
        assert_eq!(decision.kind, EngineKind::Full);
        assert_eq!(decision.rule, RULE_CONFIG);

        let sub = select(&permintaan("www.toko.test"), &config, &semua()).expect("subdomain cocok");
        assert_eq!(sub.kind, EngineKind::Full);

        // Bukan subdomain: "tokotest" tidak boleh ikut cocok, jadi permintaan
        // teks-saja tetap jatuh ke aturan termurah, bukan ke chrome-full.
        let lain = select(&permintaan("tokotest"), &config, &semua()).expect("harus dipilih");
        assert_ne!(lain.kind, EngineKind::Full, "aturan host tidak boleh bocor");
        assert_eq!(lain.rule, RULE_CHEAPEST);
    }

    #[test]
    fn host_stealth_tanpa_full_ditolak_bukan_jatuh_ke_shell() {
        let config = PolicySection {
            stealth_hosts: vec!["cloudflare.com".to_owned()],
            ..PolicySection::default()
        };
        let katalog = Catalog::with_shell(Capabilities {
            layout: true,
            script: true,
            cookies: true,
            input: true,
        });
        let err = select(&permintaan("challenges.cloudflare.com"), &config, &katalog)
            .expect_err("harus ditolak");
        assert_eq!(
            err,
            SelectError::StealthHostNeedsFull {
                host: "challenges.cloudflare.com".to_owned()
            }
        );
    }

    #[test]
    fn host_stealth_memakai_full_kalau_terpasang() {
        let config = PolicySection {
            stealth_hosts: vec!["cloudflare.com".to_owned()],
            ..PolicySection::default()
        };
        let decision = select(&permintaan("cloudflare.com"), &config, &semua()).expect("dipilih");
        assert_eq!(decision.kind, EngineKind::Full);
        assert_eq!(decision.rule, RULE_STEALTH);
    }

    #[test]
    fn pdf_dan_screenshot_memilih_mesin_berlayout() {
        let request = Request {
            wants_pdf: true,
            ..permintaan("contoh.test")
        };
        let decision = select(&request, &PolicySection::default(), &semua()).expect("dipilih");
        assert_eq!(decision.kind, EngineKind::Shell);
        assert_eq!(decision.rule, RULE_PDF);

        let request = Request {
            wants_screenshot: true,
            ..permintaan("contoh.test")
        };
        let decision = select(&request, &PolicySection::default(), &semua()).expect("dipilih");
        assert_eq!(decision.kind, EngineKind::Shell);
        assert_eq!(decision.rule, RULE_LAYOUT);
    }

    #[test]
    fn teks_saja_memilih_mesin_termurah() {
        let decision = select(
            &permintaan("contoh.test"),
            &PolicySection::default(),
            &semua(),
        )
        .expect("dipilih");
        assert_eq!(decision.kind, EngineKind::Oxi);
        assert_eq!(decision.rule, RULE_CHEAPEST);
    }

    #[test]
    fn tanpa_oxi_teks_saja_jatuh_ke_shell() {
        let mut katalog = semua();
        katalog.oxi = None;
        let decision = select(
            &permintaan("contoh.test"),
            &PolicySection::default(),
            &katalog,
        )
        .expect("dipilih");
        assert_eq!(decision.kind, EngineKind::Shell);
        assert_eq!(decision.rule, RULE_CHEAPEST);
    }

    #[test]
    fn kebutuhan_layout_tanpa_mesin_berlayout_menyebut_kemampuan_yang_kurang() {
        let katalog = Catalog {
            shell: None,
            full: None,
            oxi: Some(Capabilities {
                layout: false,
                script: true,
                cookies: true,
                input: true,
            }),
        };
        let request = Request {
            wants_screenshot: true,
            ..permintaan("contoh.test")
        };
        let err = select(&request, &PolicySection::default(), &katalog)
            .expect_err("tidak ada mesin berlayout");
        assert_eq!(
            err,
            SelectError::NoEngineWithCapabilities {
                missing: vec!["layout"]
            }
        );
    }

    #[test]
    fn nama_mesin_di_config_yang_salah_disebut() {
        let config = PolicySection {
            rules: BTreeMap::from([("contoh.test".to_owned(), "firefox".to_owned())]),
            ..PolicySection::default()
        };
        let err = select(&permintaan("contoh.test"), &config, &semua()).expect_err("harus ditolak");
        assert_eq!(err, SelectError::UnknownEngineName("firefox".to_owned()));
    }

    #[test]
    fn pencocokan_host_pada_label_domain() {
        assert!(host_matches("cloudflare.com", "cloudflare.com"));
        assert!(host_matches("cloudflare.com", "challenges.cloudflare.com"));
        assert!(host_matches("*.cloudflare.com", "a.b.cloudflare.com"));
        assert!(!host_matches("cloudflare.com", "evilcloudflare.com"));
        assert!(!host_matches("cloudflare.com", "cloudflare.com.evil.test"));
        assert!(!host_matches("", "cloudflare.com"));
    }

    #[test]
    fn bagian_config_bawaan_sesuai_dokumen() {
        let policy = PolicySection::default();
        // Tanpa `[policy] default` tidak ada nama mesin yang dikarang di sini:
        // mesin bawaan ditentukan pemanggil dari konfigurasinya sendiri.
        assert_eq!(policy.default, None);
        assert!(policy.stealth_hosts.is_empty());

        let captcha = CaptchaSection::default();
        assert_eq!(captcha.handoff_per_host_hour, 5);
        assert_eq!(captcha.handoff_total_day, 20);
        assert_eq!(captcha.cooldown_secs, 600);
    }

    #[test]
    fn toml_policy_dan_captcha_terbaca() {
        let text = r#"
[policy]
default = "chrome-full"
stealth_hosts = ["cloudflare.com"]

[policy.rules]
"shop.example" = "chrome-full"

[captcha]
handoff_per_host_hour = 3
handoff_total_day = 10
cooldown_secs = 120
"#;
        #[derive(Deserialize)]
        struct Root {
            #[serde(default)]
            policy: PolicySection,
            #[serde(default)]
            captcha: CaptchaSection,
        }
        let root: Root = toml::from_str(text).expect("harus bisa diparse");
        assert_eq!(root.policy.default.as_deref(), Some("chrome-full"));
        assert!(root.policy.is_stealth_host("static.cloudflare.com"));
        assert_eq!(root.policy.rule_for("shop.example"), Some("chrome-full"));
        assert_eq!(root.captcha.handoff_per_host_hour, 3);
    }

    #[test]
    fn katalog_dari_mesin_hanya_berisi_yang_terpasang() {
        // Daemon membangun katalog dari mesin yang benar-benar dinyalakan.
        let katalog = Catalog::dari_mesin(["chrome-shell"]);
        assert!(katalog.shell.is_some());
        assert!(
            katalog.full.is_none(),
            "full tidak dipasang, bukan diasumsikan"
        );
        let katalog = Catalog::dari_mesin(["chrome_shell", "chrome_full", "oxi", "ngawur"]);
        assert!(katalog.shell.is_some());
        assert!(katalog.full.is_some());
        assert!(katalog.oxi.is_some());
    }

    #[test]
    fn validasi_aturan_yang_menunjuk_mesin_hilang_menyebut_namanya() {
        // AC-008: aturan yang menyebut mesin tidak terpasang ditolak saat
        // start, dan pesannya menyebut mesin itu.
        let policy = PolicySection {
            default: Some("chrome-shell".to_owned()),
            stealth_hosts: Vec::new(),
            rules: BTreeMap::from([("shop.example".to_owned(), "chrome-full".to_owned())]),
        };
        let salah = policy
            .validasi(&Catalog::dari_mesin(["chrome-shell"]))
            .expect_err("aturan ke mesin hilang harus ditolak");
        assert_eq!(salah.len(), 1);
        let pesan = salah[0].to_string();
        assert!(pesan.contains("chrome-full"), "{pesan}");
        assert!(pesan.contains("host_rule"), "{pesan}");
        assert!(pesan.contains("tidak terpasang"), "{pesan}");
    }

    #[test]
    fn validasi_aturan_sah_lolos_dan_mesin_hilang_tidak_menghalangi() {
        let policy = PolicySection {
            default: Some("chrome-shell".to_owned()),
            stealth_hosts: vec!["static.cloudflare.com".to_owned()],
            rules: BTreeMap::from([("shop.example".to_owned(), "chrome-full".to_owned())]),
        };
        assert!(policy.validasi(&semua()).is_ok());

        // Tanpa chrome-full, host stealth dan aturan host sama-sama dilaporkan
        // dalam satu jalan, bukan satu per satu.
        let salah = policy
            .validasi(&Catalog::dari_mesin(["chrome-shell"]))
            .expect_err("dua pelanggaran harus dilaporkan");
        assert_eq!(salah.len(), 2, "{salah:?}");
    }

    #[test]
    fn validasi_nama_mesin_salah_disebut_namanya() {
        let policy = PolicySection {
            default: Some("chrome-shel".to_owned()),
            stealth_hosts: Vec::new(),
            rules: BTreeMap::from([("a.example".to_owned(), "krome-full".to_owned())]),
        };
        let salah = policy
            .validasi(&semua())
            .expect_err("nama salah harus ditolak");
        let pesan = format!("{salah:?}");
        assert!(pesan.contains("chrome-shel"), "{pesan}");
        assert!(pesan.contains("krome-full"), "{pesan}");
    }

    // AC-007: jalur protokol cuma tahu host. Yang diputuskan di sini hanya
    // aturan host; sisanya menyerahkan keputusan ke pemanggil.
    #[test]
    fn host_dengan_aturan_diarahkan_ke_mesin_yang_ditunjuk() {
        let config = PolicySection {
            rules: BTreeMap::from([("toko.test".to_owned(), "chrome-full".to_owned())]),
            ..PolicySection::default()
        };
        let keputusan = select_untuk_host("toko.test", &config, &semua())
            .expect("aturan sah")
            .expect("ada aturan yang berlaku");
        assert_eq!(keputusan.kind, EngineKind::Full);
        assert_eq!(keputusan.rule, RULE_CONFIG);

        let sub = select_untuk_host("www.toko.test", &config, &semua())
            .expect("aturan sah")
            .expect("subdomain ikut aturan");
        assert_eq!(sub.kind, EngineKind::Full);
    }

    #[test]
    fn host_tanpa_aturan_tidak_memutuskan_apa_apa() {
        // Kalau ini mengembalikan mesin termurah, SELURUH trafik yang hostnya
        // tidak disebut di config pindah mesin tanpa ada yang menyuruh.
        assert_eq!(
            select_untuk_host("lain.test", &PolicySection::default(), &semua())
                .expect("tanpa aturan bukan galat"),
            None
        );

        // Port bukan bagian dari host: aturan ditulis tanpa port, jadi
        // "toko.test:8080" bukan "toko.test" dan tidak boleh cocok.
        let config = PolicySection {
            rules: BTreeMap::from([("toko.test".to_owned(), "chrome-full".to_owned())]),
            ..PolicySection::default()
        };
        assert_eq!(
            select_untuk_host("toko.test:8080", &config, &semua())
                .expect("tanpa aturan bukan galat"),
            None
        );
    }

    #[test]
    fn host_stealth_diarahkan_ke_full() {
        let config = PolicySection {
            stealth_hosts: vec!["cloudflare.com".to_owned()],
            ..PolicySection::default()
        };
        let keputusan = select_untuk_host("challenges.cloudflare.com", &config, &semua())
            .expect("aturan sah")
            .expect("host stealth punya aturan");
        assert_eq!(keputusan.kind, EngineKind::Full);
        assert_eq!(keputusan.rule, RULE_STEALTH);
    }

    #[test]
    fn aturan_host_yang_tidak_bisa_dipenuhi_ditolak() {
        let config = PolicySection {
            rules: BTreeMap::from([("toko.test".to_owned(), "chrome-full".to_owned())]),
            ..PolicySection::default()
        };
        let katalog = Catalog::dari_mesin(["chrome-shell"]);
        let err =
            select_untuk_host("toko.test", &config, &katalog).expect_err("chrome-full tidak ada");
        assert!(
            err.to_string().contains("chrome-full"),
            "pesan galat menyebut mesinnya: {err}"
        );

        let stealth = PolicySection {
            stealth_hosts: vec!["cloudflare.com".to_owned()],
            ..PolicySection::default()
        };
        let err = select_untuk_host("cloudflare.com", &stealth, &katalog)
            .expect_err("stealth tanpa full ditolak, bukan jatuh ke shell");
        assert!(
            matches!(err, SelectError::StealthHostNeedsFull { .. }),
            "{err:?}"
        );
    }
}
