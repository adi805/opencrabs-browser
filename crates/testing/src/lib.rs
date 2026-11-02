//! Alat uji: mesin palsu dan fixture.
//!
//! Crate ini **hanya untuk dev-dependency**. Kalau muncul di `[dependencies]`
//! crate lain, itu bug: artinya kode produksi bergantung pada alat uji.
//!
//! Gunanya nyata: endpoint discovery dan penerusan pesan bisa diuji tanpa
//! menyalakan Chromium, sehingga test cepat dan tidak butuh GPU, sandbox, atau
//! port yang bebas.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use engine::{Engine, EngineError, EngineInfo, LaunchSpec};

/// Mesin yang tidak menyalakan proses apa pun.
///
/// `devtools_port` **tidak** menunjuk ke server sungguhan: kalau ada test yang
/// lupa memakai fixture HTTP, dia akan gagal menyambung, bukan diam-diam
/// membaca data palsu yang terlihat benar.
pub struct FakeEngine {
    info: EngineInfo,
    spec: LaunchSpec,
    running: AtomicBool,
    pid: u32,
    stop_calls: AtomicU32,
    probes: AtomicU64,
}

impl Default for FakeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::at_port(1)
    }

    #[must_use]
    pub fn at_port(port: u16) -> Self {
        Self {
            info: EngineInfo {
                browser: "FakeChrome/0.0".to_owned(),
                protocol_version: "1.3".to_owned(),
                user_agent: "fake-agent".to_owned(),
                v8_version: None,
                webkit_version: None,
                devtools_port: port,
                web_socket_debugger_url: format!("ws://127.0.0.1:{port}/devtools/browser/fake"),
            },
            spec: LaunchSpec::new(PathBuf::from("/tidak/ada"), PathBuf::from("/tmp/ocb-fake")),
            running: AtomicBool::new(true),
            pid: 4242,
            stop_calls: AtomicU32::new(0),
            probes: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn stop_calls(&self) -> u32 {
        self.stop_calls.load(Ordering::SeqCst)
    }

    /// Berapa kali keadaan mesin ditanya. Dipakai untuk membuktikan bahwa
    /// endpoint kesehatan benar-benar memeriksa, bukan menjawab "ok" buta.
    #[must_use]
    pub fn probes(&self) -> u64 {
        self.probes.load(Ordering::SeqCst)
    }
}

impl Engine for FakeEngine {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn info(&self) -> &EngineInfo {
        &self.info
    }

    fn spec(&self) -> &LaunchSpec {
        &self.spec
    }

    fn http_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.info.devtools_port)
    }

    fn web_socket_url(&self) -> &str {
        &self.info.web_socket_debugger_url
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn is_running(&self) -> bool {
        self.probes.fetch_add(1, Ordering::SeqCst);
        self.running.load(Ordering::SeqCst)
    }

    fn stop(&self) -> Result<(), EngineError> {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
}
