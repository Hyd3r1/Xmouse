//! Fetching the frame generators' files on request, and checking who signed them.
//!
//! XMouse ships none of AMD's or NVIDIA's files. When the user asks for it, they are
//! downloaded from the vendors' own GitHub repositories (https only), checked for the
//! vendor's signature, and only then moved into `<config>/framegen/{fsr,dlss}`. A download
//! that fails or does not check out leaves nothing behind.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use super::Kind;

const FSR_BASE: &str = "https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/v2.4.0/Kits/FidelityFX/signedbin/";
const DLSS_ZIP: &str = "https://github.com/NVIDIA-RTX/Streamline/releases/download/v2.14.16/streamline-sdk-v2.14.16.zip";
/// Somewhere else to take the files from, for testing: a folder (FSR) or a zip file (DLSS)
/// on this PC, or another https address.
const FSR_SOURCE_ENV: &str = "XMOUSE_FSR_SOURCE";
const DLSS_SOURCE_ENV: &str = "XMOUSE_DLSS_SOURCE";

const MIB: u64 = 1024 * 1024;
const FILE_CAP: u64 = 96 * MIB;
const ZIP_CAP: u64 = 512 * MIB;

/// Where an install stands, for the settings page.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    /// Running; what it is doing right now.
    Working(String),
    Failed(String),
    Done,
}

static STATES: Mutex<[State; 2]> = Mutex::new([State::Idle, State::Idle]);

fn slot(kind: Kind) -> usize {
    match kind {
        Kind::Fsr => 0,
        Kind::Dlss => 1,
    }
}

pub fn state(kind: Kind) -> State {
    STATES.lock().unwrap()[slot(kind)].clone()
}

fn set(kind: Kind, s: State) {
    STATES.lock().unwrap()[slot(kind)] = s;
}

/// Downloads and installs one generator's files in the background. `wake` is called
/// whenever there is something new to show.
pub fn start(kind: Kind, config_dir: PathBuf, rt: &tokio::runtime::Runtime, wake: impl Fn() + Send + Sync + 'static) {
    {
        let mut st = STATES.lock().unwrap();
        if matches!(st[slot(kind)], State::Working(_)) {
            return;
        }
        st[slot(kind)] = State::Working("Starting…".into());
    }
    rt.spawn(async move {
        let res = run(kind, &config_dir, source(kind), &wake).await;
        match res {
            Ok(()) => {
                log::info!("{} frame generation files installed", kind.name());
                set(kind, State::Done);
            }
            Err(e) => {
                log::warn!("{} frame generation install: {e:#}", kind.name());
                set(kind, State::Failed(format!("{e:#}")));
            }
        }
        wake();
    });
}

/// Removes a generator's files again.
pub fn remove(kind: Kind, config_dir: &Path) -> Result<()> {
    let dir = config_dir.join("framegen").join(kind.folder());
    if dir.exists() {
        std::fs::remove_dir_all(&dir).context("the files are in use; end the session first")?;
    }
    set(kind, State::Idle);
    Ok(())
}

/// Where the files come from: the maker's page, unless a test points elsewhere.
fn source(kind: Kind) -> String {
    match kind {
        Kind::Fsr => std::env::var(FSR_SOURCE_ENV).unwrap_or_else(|_| FSR_BASE.to_string()),
        Kind::Dlss => std::env::var(DLSS_SOURCE_ENV).unwrap_or_else(|_| DLSS_ZIP.to_string()),
    }
}

async fn run(kind: Kind, config_dir: &Path, source: String, wake: &(impl Fn() + Send + Sync)) -> Result<()> {
    let root = config_dir.join("framegen");
    let staging = root.join(format!(".install-{}-{}", kind.folder(), std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let res = fill(kind, &staging, source, wake).await.and_then(|()| {
        let (files, signer) = kind.files();
        set(kind, State::Working("Checking the signatures…".into()));
        wake();
        for f in files {
            verify_signature(&staging.join("new").join(f), signer)?;
        }
        // Swap: the old folder steps aside, the new one takes its place.
        let dest = root.join(kind.folder());
        if dest.exists() {
            std::fs::rename(&dest, staging.join("old")).context("the old files are in use; end the session first")?;
        }
        std::fs::rename(staging.join("new"), &dest).context("could not put the files in place")
    });
    let _ = std::fs::remove_dir_all(&staging);
    res
}

/// Puts the wanted files into `staging/new`.
async fn fill(kind: Kind, staging: &Path, source: String, wake: &(impl Fn() + Send + Sync)) -> Result<()> {
    let new = staging.join("new");
    std::fs::create_dir_all(&new)?;
    let progress = |what: &str, got: u64, total: Option<u64>| {
        let text = match total {
            Some(t) if t > 0 => format!("{what}: {} of {} MB", got / MIB, t.div_ceil(MIB)),
            _ => format!("{what}: {} MB", got / MIB),
        };
        set(kind, State::Working(text));
        wake();
    };
    match kind {
        Kind::Fsr => {
            let base = source;
            for f in kind.files().0 {
                let to = new.join(f);
                if base.starts_with("https://") {
                    download(&format!("{}/{f}", base.trim_end_matches('/')), &to, FILE_CAP, &|g, t| progress("Downloading from AMD", g, t)).await?;
                } else {
                    std::fs::copy(Path::new(&base).join(f), &to).with_context(|| format!("copying {f}"))?;
                }
            }
        }
        Kind::Dlss => {
            let zip_path = if source.starts_with("https://") {
                let to = staging.join("download.zip");
                download(&source, &to, ZIP_CAP, &|g, t| progress("Downloading from NVIDIA", g, t)).await?;
                to
            } else {
                PathBuf::from(source)
            };
            set(kind, State::Working("Unpacking…".into()));
            wake();
            let wanted = kind.files().0;
            tokio::task::spawn_blocking(move || unpack(&zip_path, &new, wanted)).await??;
        }
    }
    Ok(())
}

/// Takes only the named production libraries out of the SDK archive (`…/bin/x64/<name>`).
fn unpack(zip_path: &Path, into: &Path, wanted: &[&str]) -> Result<()> {
    let file = std::fs::File::open(zip_path).context("opening the download")?;
    let mut zip = zip::ZipArchive::new(file).context("the download is not a zip file")?;
    for name in wanted {
        let suffix = format!("bin/x64/{name}");
        let index = (0..zip.len())
            .find(|&i| zip.name_for_index(i).and_then(|n| n.ok()).is_some_and(|n| n == suffix.as_str() || n.ends_with(&format!("/{suffix}"))))
            .ok_or_else(|| anyhow!("{name} is not in the download"))?;
        let mut entry = zip.by_index(index)?;
        if entry.size() > FILE_CAP {
            bail!("{name} is larger than expected");
        }
        let mut out = std::fs::File::create(into.join(name))?;
        let copied = std::io::copy(&mut (&mut entry).take(FILE_CAP + 1), &mut out)?;
        if copied > FILE_CAP {
            bail!("{name} is larger than expected");
        }
    }
    Ok(())
}

async fn download(url: &str, to: &Path, cap: u64, progress: &(impl Fn(u64, Option<u64>) + Sync)) -> Result<()> {
    let client = reqwest::Client::builder()
        .https_only(true) // also for every redirect on the way to the file
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(45 * 60))
        .build()?;
    let mut response = client.get(url).send().await.context("no connection to the server")?;
    if !response.status().is_success() {
        bail!("the server answered with error {}", response.status().as_u16());
    }
    let total = response.content_length();
    if total.is_some_and(|t| t > cap) {
        bail!("the file is larger than expected");
    }
    let mut file = std::fs::File::create(to)?;
    let (mut got, mut shown) = (0u64, 0u64);
    while let Some(chunk) = response.chunk().await.context("the download broke off")? {
        got += chunk.len() as u64;
        if got > cap {
            bail!("the file is larger than expected");
        }
        file.write_all(&chunk)?;
        if got - shown >= MIB {
            shown = got;
            progress(got, total);
        }
    }
    file.flush()?;
    Ok(())
}

/// Checks that Windows trusts the file's signature and that the signer's name contains
/// `signer`. Nothing from a vendor is loaded before this has passed.
pub fn verify_signature(path: &Path, signer: &str) -> Result<()> {
    use std::ffi::c_void;
    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::Security::Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE};
    use windows::Win32::Security::WinTrust::*;

    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut file = WINTRUST_FILE_INFO {
            cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(wide.as_ptr()),
            hFile: HANDLE::default(),
            pgKnownSubject: std::ptr::null_mut(),
        };
        let mut data = WINTRUST_DATA {
            cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
            dwStateAction: WTD_STATEACTION_VERIFY,
            dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
            ..Default::default()
        };
        // The standard Authenticode policy.
        let mut action = GUID::from_u128(0x00aac56b_cd44_11d0_8cc2_00c04fc295ee);
        let status = WinVerifyTrust(HWND::default(), &mut action, &mut data as *mut _ as *mut c_void);
        let mut who = String::new();
        if status == 0 {
            let prov = WTHelperProvDataFromStateData(data.hWVTStateData);
            if !prov.is_null() {
                let sgnr = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
                if !sgnr.is_null() {
                    let cert = WTHelperGetProvCertFromChain(sgnr, 0);
                    if !cert.is_null() && !(*cert).pCert.is_null() {
                        let mut buf = [0u16; 256];
                        let n = CertGetNameStringW((*cert).pCert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf));
                        who = String::from_utf16_lossy(&buf[..(n as usize).saturating_sub(1)]);
                    }
                }
            }
        }
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(HWND::default(), &mut action, &mut data as *mut _ as *mut c_void);
        if status != 0 {
            bail!("{name} has no valid signature (Windows error {status:#x})");
        }
        if !who.to_lowercase().contains(&signer.to_lowercase()) {
            bail!("{name} is signed by \"{who}\", not by {signer}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xmouse-fg-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// The real install from the makers' pages (`cargo test real_install -- --ignored`).
    #[tokio::test]
    #[ignore]
    async fn real_install_from_the_makers_pages() {
        for kind in [Kind::Fsr, Kind::Dlss] {
            let d = scratch("real");
            let src = match kind {
                Kind::Fsr => FSR_BASE,
                Kind::Dlss => DLSS_ZIP,
            };
            run(kind, &d, src.to_string(), &|| {}).await.unwrap();
            let dir = d.join("framegen").join(kind.folder());
            let (files, signer) = kind.files();
            for f in files {
                verify_signature(&dir.join(f), signer).unwrap();
            }
            let left: Vec<_> = std::fs::read_dir(d.join("framegen")).unwrap().flatten().map(|e| e.file_name()).collect();
            assert_eq!(left.len(), 1, "staging left behind: {left:?}");
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn unpack_takes_only_the_named_production_files() {
        let d = scratch("unpack");
        let zip = d.join("sdk.zip");
        std::fs::write(
            &zip,
            zip_of(&[
                ("sdk/bin/x64/development/a.dll", b"dev"),
                ("sdk/bin/x64/a.dll", b"good"),
                ("sdk/bin/x64/other.dll", b"other"),
                ("sdk/include/a.dll", b"wrong place"),
            ]),
        )
        .unwrap();
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        unpack(&zip, &out, &["a.dll"]).unwrap();
        assert_eq!(std::fs::read(out.join("a.dll")).unwrap(), b"good");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
        assert!(unpack(&zip, &out, &["missing.dll"]).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unsigned_file_is_refused() {
        let d = scratch("sig");
        let f = d.join("fake.dll");
        std::fs::write(&f, b"MZ not really a library").unwrap();
        assert!(verify_signature(&f, "NVIDIA").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_windows_library_is_not_taken_for_a_vendor_one() {
        // Properly signed, but by somebody else.
        let sys = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())).join("explorer.exe");
        if sys.exists() {
            assert!(verify_signature(&sys, "NVIDIA").is_err());
        }
    }

    /// The vendors' real files, when this PC has them next to the repository (they are
    /// never in it): the whole install, without the network.
    fn local_sdks() -> Option<PathBuf> {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("sdks");
        (d.join("ffx").join("amd_fidelityfx_loader_dx12.dll").is_file()
            && d.join("streamline/sdk/bin/x64/sl.interposer.dll").is_file())
        .then_some(d)
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn installs_signed_files_and_refuses_tampered_ones() {
        let Some(sdks) = local_sdks() else { return };
        let d = scratch("install");
        // FSR from a folder; twice, so the second run replaces the first.
        for _ in 0..2 {
            block_on(run(Kind::Fsr, &d, sdks.join("ffx").to_string_lossy().to_string(), &|| {})).unwrap();
        }
        let fsr = d.join("framegen").join("fsr");
        assert!(Kind::Fsr.files().0.iter().all(|f| fsr.join(f).is_file()));
        // DLSS from an archive laid out like NVIDIA's, with more in it than we want.
        let bin = sdks.join("streamline/sdk/bin/x64");
        let mut w = zip::ZipWriter::new(std::fs::File::create(d.join("sdk.zip")).unwrap());
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for f in Kind::Dlss.files().0.iter().chain(&["sl.nis.dll"]) {
            w.start_file(format!("streamline-sdk/bin/x64/{f}"), stored).unwrap();
            w.write_all(&std::fs::read(bin.join(f)).unwrap()).unwrap();
        }
        w.finish().unwrap();
        block_on(run(Kind::Dlss, &d, d.join("sdk.zip").to_string_lossy().to_string(), &|| {})).unwrap();
        let dlss = d.join("framegen").join("dlss");
        assert_eq!(std::fs::read_dir(&dlss).unwrap().count(), Kind::Dlss.files().0.len());
        // A changed file fails the signature check and leaves the good install alone.
        let bad = d.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        for f in Kind::Fsr.files().0 {
            let mut bytes = std::fs::read(sdks.join("ffx").join(f)).unwrap();
            let mid = bytes.len() / 2;
            bytes[mid] ^= 0xff;
            std::fs::write(bad.join(f), bytes).unwrap();
        }
        let before = std::fs::metadata(fsr.join("amd_fidelityfx_loader_dx12.dll")).unwrap().modified().unwrap();
        assert!(block_on(run(Kind::Fsr, &d, bad.to_string_lossy().to_string(), &|| {})).is_err());
        assert_eq!(std::fs::metadata(fsr.join("amd_fidelityfx_loader_dx12.dll")).unwrap().modified().unwrap(), before);
        assert!(std::fs::read_dir(d.join("framegen")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".install")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dll_dir_wants_every_file() {
        let d = scratch("dir");
        let fsr = d.join("framegen").join("fsr");
        std::fs::create_dir_all(&fsr).unwrap();
        std::fs::write(fsr.join("amd_fidelityfx_loader_dx12.dll"), b"x").unwrap();
        if std::env::var_os("XMOUSE_FSR_DIR").is_none() {
            assert!(super::super::dll_dir(Kind::Fsr, &d).is_none());
            std::fs::write(fsr.join("amd_fidelityfx_framegeneration_dx12.dll"), b"x").unwrap();
            assert_eq!(super::super::dll_dir(Kind::Fsr, &d), Some(fsr));
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
