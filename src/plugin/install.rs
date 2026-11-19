//! Installing a plugin from its release zip (the "Install PSNBridge" button).
//!
//! A plugin is a program XMouse will later start, so everything here is treated as outside
//! input: the download is https only and bounded, the zip is checked name by name before a
//! single byte is written, and the files only reach the plugins folder after the unpacked
//! copy passed the same manifest checks every installed plugin passes. Nothing downloaded is
//! run here.
//!
//! # The release zip
//! `xmouse-<id>.zip` holds one folder named after the plugin id, so extracting it by hand
//! into the plugins folder is a complete manual install:
//! ```text
//! psnbridge/plugin.json            required
//! psnbridge/xmouse-psnbridge.exe   required (the manifest's "executable")
//! psnbridge/LICENSE                optional
//! psnbridge/README.md              optional
//! ```
//! The same files directly at the root of the zip (no folder) are accepted too. Nothing
//! else is: no other or deeper folders, no links, no file called `data` (that name belongs to
//! the plugin's own saved files), at most 32 entries. File names are plain ASCII letters,
//! digits, `.`, `-` and `_`, separated by `/` (never `\`).
//!
//! # How an install goes
//! 1. Everything is unpacked into `<plugins>/.install-<random>/new/<id>/`.
//! 2. That folder is checked with [`manifest::load`]; its id must be the expected one.
//! 3. An installed `<plugins>/<id>` is moved to `.install-<random>/old/<id>`, its `data`
//!    folder is moved into the new copy, and the new copy is moved to `<plugins>/<id>`.
//!    If any step fails the earlier ones are undone, so the old plugin stays as it was.
//! 4. `.install-<random>` is deleted, on success and on failure.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use super::manifest::{self, PluginInfo};

pub const PSNBRIDGE_ID: &str = "psnbridge";
pub const PSNBRIDGE_NAME: &str = "PSNBridge";

const PSNBRIDGE_URL: &str =
    "https://github.com/thedarkcr0w/xmouse-psnbridge/releases/latest/download/xmouse-psnbridge.zip";
/// Replaces the download address, for testing. May also be the path of a zip on this computer.
const SOURCE_ENV: &str = "XMOUSE_PSNBRIDGE_URL";

const MIB: u64 = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REDIRECTS: usize = 5;
/// Staging folders are named `.install-<random>`; the dot keeps discovery away from them.
const STAGING_PREFIX: &str = ".install-";
/// A staging folder this old was left behind by an install that never finished.
const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// How much a package may hold. Tests use smaller numbers.
#[derive(Clone, Copy)]
struct Limits {
    /// The zip itself (also the download cap).
    zip: u64,
    entries: usize,
    /// One unpacked file.
    entry: u64,
    /// All unpacked files together.
    total: u64,
}

const LIMITS: Limits = Limits { zip: 64 * MIB, entries: 32, entry: 64 * MIB, total: 128 * MIB };

/// One install at a time in this process: two would fight over the same plugin folder.
static INSTALLING: Mutex<()> = Mutex::new(());

/// Why an install stopped. Turned into the text the user sees at the very end.
enum Fail {
    /// The installed plugin can't be replaced because its program is in use.
    Running,
    Other(String),
}

impl From<String> for Fail {
    fn from(why: String) -> Self {
        Fail::Other(why)
    }
}

impl From<&str> for Fail {
    fn from(why: &str) -> Self {
        Fail::Other(why.to_string())
    }
}

/// Where PSNBridge is downloaded from:
/// <https://github.com/thedarkcr0w/xmouse-psnbridge/releases/latest/download/xmouse-psnbridge.zip>,
/// or the value of the environment variable `XMOUSE_PSNBRIDGE_URL` (for testing; that one may
/// also be a local file path).
pub fn psnbridge_source() -> String {
    match std::env::var(SOURCE_ENV) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => PSNBRIDGE_URL.to_string(),
    }
}

/// Download PSNBridge (https only, or a local path from the environment override) and install
/// it into `plugins_dir`. Must be called from a tokio task.
pub async fn install_psnbridge(plugins_dir: &Path) -> Result<PluginInfo, String> {
    let source = psnbridge_source();
    if source != PSNBRIDGE_URL {
        log::warn!("{PSNBRIDGE_NAME} is being installed from {source} ({SOURCE_ENV} is set)");
    }
    let bytes = fetch(&source, LIMITS.zip)
        .await
        .map_err(|why| format!("Couldn't download {PSNBRIDGE_NAME}: {why}"))?;
    let dir = plugins_dir.to_path_buf();
    // Unpacking is ordinary blocking file work.
    tokio::task::spawn_blocking(move || install_zip(&bytes, &dir, PSNBRIDGE_ID))
        .await
        .map_err(|e| {
            log::error!("the install task failed: {e}");
            format!("Couldn't install {PSNBRIDGE_NAME}: the installer stopped unexpectedly")
        })?
}

/// Install a release zip, given as bytes, as the plugin `expected_id` (see the module text
/// for the layout and the steps). On an error the plugins folder is as it was before.
pub fn install_zip(bytes: &[u8], plugins_dir: &Path, expected_id: &str) -> Result<PluginInfo, String> {
    install_with(bytes, plugins_dir, expected_id, &LIMITS)
}

fn install_with(
    bytes: &[u8],
    plugins_dir: &Path,
    id: &str,
    limits: &Limits,
) -> Result<PluginInfo, String> {
    let label = label(id);
    match install_inner(bytes, plugins_dir, id, limits) {
        Ok(info) => {
            log::info!("installed plugin {} {} into {}", info.id, info.version, info.dir.display());
            Ok(info)
        }
        Err(Fail::Running) => Err(format!("{label} is running – stop playing first")),
        Err(Fail::Other(why)) => Err(format!("Couldn't install {label}: {why}")),
    }
}

/// What the plugin is called in messages.
fn label(id: &str) -> String {
    if id == PSNBRIDGE_ID {
        PSNBRIDGE_NAME.to_string()
    } else {
        format!("plugin \"{}\"", printable(id))
    }
}

/// The same rule the manifest has for ids: the id becomes a folder name.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn install_inner(bytes: &[u8], plugins_dir: &Path, id: &str, limits: &Limits) -> Result<PluginInfo, Fail> {
    if !valid_id(id) {
        return Err("that is not a valid plugin id".into());
    }
    let _one_at_a_time = INSTALLING.lock().unwrap_or_else(|e| e.into_inner());
    fs::create_dir_all(plugins_dir).map_err(|e| format!("can't create the plugins folder ({e})"))?;
    sweep_stale(plugins_dir, STALE_AFTER);

    let mut staging = Staging::create(plugins_dir)?;
    let new_dir = staging.root.join("new").join(id);
    fs::create_dir_all(&new_dir).map_err(|e| format!("can't write to the plugins folder ({e})"))?;
    unpack(bytes, id, &new_dir, limits)?;

    let mut info = manifest::load(&new_dir).map_err(|why| {
        if why.starts_with("executable not found") {
            "the package does not contain the plugin's program".to_string()
        } else {
            format!("the package is not a usable plugin ({why})")
        }
    })?;
    if info.id != id {
        return Err("the package is a different plugin".into());
    }

    let dest = plugins_dir.join(id);
    swap_in(&mut staging, &new_dir, &dest, id)?;

    // The checked copy now lives at `dest`: same files, new address.
    let exe_rel = info
        .exe
        .strip_prefix(&new_dir)
        .map(Path::to_path_buf)
        .map_err(|_| "the plugin's program is not inside its folder".to_string())?;
    info.exe = dest.join(exe_rel);
    info.data_dir = dest.join("data");
    info.dir = dest;
    Ok(info)
}

// ---------------------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------------------

/// `<plugins>/.install-<random>`, deleted when dropped.
struct Staging {
    root: PathBuf,
    /// Set when a failed roll-back left the user's old plugin in here: then it is not deleted.
    keep: bool,
}

impl Staging {
    fn create(plugins_dir: &Path) -> Result<Self, String> {
        let root = plugins_dir.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        // `create_dir`, not `create_dir_all`: the folder must be new and ours.
        fs::create_dir(&root).map_err(|e| format!("can't write to the plugins folder ({e})"))?;
        Ok(Self { root, keep: false })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Err(e) = fs::remove_dir_all(&self.root) {
            if fs::symlink_metadata(&self.root).is_ok() {
                log::warn!("couldn't delete {}: {e}", self.root.display());
            }
        }
    }
}

/// Deal with staging folders an earlier install left behind (XMouse was closed or crashed in
/// the middle): a plugin that was moved aside and never replaced goes back to its place, and
/// the rest is deleted. Folders younger than `older_than` are left alone, they may belong to
/// an install that is running right now in another XMouse.
fn sweep_stale(plugins_dir: &Path, older_than: Duration) {
    let Ok(entries) = fs::read_dir(plugins_dir) else { return };
    for entry in entries.flatten().take(256) {
        let name = entry.file_name();
        let is_staging = name.to_str().is_some_and(|n| n.starts_with(STAGING_PREFIX));
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        if !is_staging || !meta.is_dir() {
            continue;
        }
        let age = meta.modified().ok().and_then(|t| t.elapsed().ok());
        if !age.is_some_and(|a| a >= older_than) {
            continue;
        }
        let root = entry.path();
        let mut safe_to_delete = true;
        if let Ok(olds) = fs::read_dir(root.join("old")) {
            for old in olds.flatten().take(8) {
                let Some(id) = old.file_name().to_str().map(str::to_string) else { continue };
                let home = plugins_dir.join(&id);
                if !valid_id(&id) || fs::symlink_metadata(&home).is_ok() {
                    continue; // a newer copy is installed: the old one is just leftovers
                }
                match fs::rename(old.path(), &home) {
                    Ok(()) => log::warn!("put plugin {id} back after an unfinished install"),
                    Err(e) => {
                        log::warn!("couldn't put plugin {id} back from {}: {e}", root.display());
                        safe_to_delete = false;
                    }
                }
            }
        }
        if safe_to_delete {
            if let Err(e) = fs::remove_dir_all(&root) {
                log::warn!("couldn't delete {}: {e}", root.display());
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Unpacking
// ---------------------------------------------------------------------------------------

/// Shorten and clean a name from the package before it goes into a message or the log.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(40).collect()
}

/// A file name that means the same thing on every system and can't point anywhere else:
/// ASCII letters, digits, `.`, `-`, `_`; starts with a letter or digit; doesn't end in a dot;
/// isn't one of Windows' device names.
fn safe_file_name(name: &str) -> bool {
    const DEVICES: [&str; 8] = ["con", "prn", "aux", "nul", "com", "lpt", "conin$", "conout$"];
    let ok_chars = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    let first_ok = name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
    if !ok_chars || !first_ok || name.ends_with('.') {
        return false;
    }
    // "NUL", "nul.txt", "COM1", "lpt9.log" all open a device on Windows.
    let stem = name.split('.').next().unwrap_or_default().to_ascii_lowercase();
    let numbered = stem.len() == 4
        && (stem.starts_with("com") || stem.starts_with("lpt"))
        && stem.ends_with(|c: char| c.is_ascii_digit());
    !(numbered || DEVICES.contains(&stem.as_str()))
}

/// Check every entry of the zip, then write the files into `into` (an empty folder).
fn unpack(bytes: &[u8], id: &str, into: &Path, limits: &Limits) -> Result<(), String> {
    const S_IFMT: u32 = 0o170000;
    const S_IFREG: u32 = 0o100000;
    const S_IFDIR: u32 = 0o040000;

    if bytes.len() as u64 > limits.zip {
        return Err("the package is too large".into());
    }
    let damaged = |e: zip::result::ZipError| {
        log::warn!("plugin package refused: {e}");
        "the package is damaged or not a zip file".to_string()
    };
    let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(damaged)?;
    if archive.is_empty() {
        return Err("the package is empty".into());
    }
    if archive.len() > limits.entries {
        return Err("the package has too many files".into());
    }
    if archive.has_duplicate_names() {
        return Err("the package lists the same file twice".into());
    }
    // Entries that share bytes are how a small zip unpacks into a huge amount of data.
    if archive.has_overlapping_entries().map_err(damaged)? {
        return Err("the package is damaged or not a zip file".into());
    }

    // First pass: decide about every name before anything is written.
    let folder_prefix = format!("{id}/");
    let mut files: Vec<(usize, String)> = Vec::new();
    let mut seen = HashSet::new();
    let mut in_folder: Option<bool> = None;
    let mut declared = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index_data(index).map_err(damaged)?;
        let name = entry.name().map_err(damaged)?.into_owned();
        let unsafe_name = || format!("the package has a file it shouldn't have ({})", printable(&name));
        let kind = entry.unix_mode().map(|mode| mode & S_IFMT).unwrap_or(0);
        if entry.is_symlink() || entry.encrypted() {
            return Err(unsafe_name());
        }
        // The one folder entry that may exist is the plugin's own folder.
        if name == folder_prefix {
            if kind != 0 && kind != S_IFDIR {
                return Err(unsafe_name());
            }
            if in_folder.replace(true) == Some(false) {
                return Err(unsafe_name());
            }
            continue;
        }
        if entry.is_dir() || (kind != 0 && kind != S_IFREG) {
            return Err(unsafe_name());
        }
        // Either "<file>" or "<id>/<file>", and the same choice for every file. Anything
        // else (.., absolute paths, drive letters, backslashes, deeper folders) fails the
        // file-name check or the prefix check.
        let (inside, file_name) = match name.strip_prefix(&folder_prefix) {
            Some(rest) => (true, rest),
            None => (false, name.as_str()),
        };
        if !safe_file_name(file_name) || file_name.eq_ignore_ascii_case("data") {
            return Err(unsafe_name());
        }
        if in_folder.replace(inside).is_some_and(|before| before != inside) {
            return Err(unsafe_name());
        }
        // Windows file names ignore case: "Plugin.json" would overwrite "plugin.json".
        if !seen.insert(file_name.to_ascii_lowercase()) {
            return Err("the package lists the same file twice".into());
        }
        declared = declared.saturating_add(entry.size());
        if entry.size() > limits.entry || declared > limits.total {
            return Err("the package unpacks to too much data".into());
        }
        files.push((index, file_name.to_string()));
    }

    // Second pass: write. The sizes above are what the zip claims; count what really comes
    // out and stop at the same limits.
    let cant_write = |e: io::Error| format!("can't write to the plugins folder ({e})");
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    for (index, file_name) in files {
        let mut src = archive.by_index(index).map_err(damaged)?;
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(into.join(&file_name))
            .map_err(cant_write)?;
        let mut written = 0u64;
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::warn!("plugin package refused: {} did not unpack: {e}", printable(&file_name));
                    return Err("the package is damaged or not a zip file".into());
                }
            };
            written += n as u64;
            total += n as u64;
            if written > limits.entry || total > limits.total {
                return Err("the package unpacks to too much data".into());
            }
            out.write_all(&buf[..n]).map_err(cant_write)?;
        }
        out.sync_all().map_err(cant_write)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Putting the new copy in place
// ---------------------------------------------------------------------------------------

/// "Somebody has this open": on Windows a folder can't be moved while a program in it runs.
fn in_use(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33)))
}

/// Rename, trying again for a moment when the folder is busy: a virus scanner looking at the
/// files that were just written holds them open for a few hundred milliseconds.
fn rename_patiently(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Err(e) if in_use(&e) && tries < 5 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            done => return done,
        }
    }
}

/// Whether a program in the installed plugin's folder is running. Windows doesn't let a
/// running program's file be opened for writing; nothing is written.
fn program_is_running(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    for entry in entries.flatten().take(256) {
        let path = entry.path();
        let is_program = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"));
        if !is_program || !path.is_file() {
            continue;
        }
        if let Err(e) = fs::OpenOptions::new().write(true).open(&path) {
            if cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)) {
                return true;
            }
        }
    }
    false
}

/// Replace `dest` with the checked folder `new_dir`, keeping `dest/data`. Either everything
/// happens or `dest` is left exactly as it was.
fn swap_in(staging: &mut Staging, new_dir: &Path, dest: &Path, id: &str) -> Result<(), Fail> {
    let had_old = fs::symlink_metadata(dest).is_ok();
    let old = staging.root.join("old").join(id);
    let mut data_moved = false;

    if had_old {
        if program_is_running(dest) {
            return Err(Fail::Running);
        }
        fs::create_dir(staging.root.join("old"))
            .map_err(|e| format!("can't write to the plugins folder ({e})"))?;
        // 1. The installed copy steps aside.
        rename_patiently(dest, &old).map_err(|e| {
            log::warn!("couldn't move {} aside: {e}", dest.display());
            if in_use(&e) {
                Fail::Running
            } else {
                Fail::Other(format!("the installed version can't be replaced ({e})"))
            }
        })?;
        // 2. Its saved data (pairing and the like) moves into the new copy. The package
        //    can't contain anything called "data", so nothing is overwritten.
        let old_data = old.join("data");
        if fs::symlink_metadata(&old_data).is_ok() {
            if let Err(e) = fs::rename(&old_data, new_dir.join("data")) {
                log::warn!("couldn't carry over {}: {e}", old_data.display());
                put_back(staging, &old, dest, None)?;
                return Err("the plugin's saved data couldn't be kept, so nothing was changed".into());
            }
            data_moved = true;
        }
    }

    // 3. The new copy moves in.
    if let Err(e) = rename_patiently(new_dir, dest) {
        log::warn!("couldn't move the new plugin to {}: {e}", dest.display());
        if had_old {
            put_back(staging, &old, dest, data_moved.then(|| new_dir.join("data")))?;
        }
        return Err(format!("can't write to the plugins folder ({e})").into());
    }
    Ok(()) // 4. the old copy is deleted with the staging folder
}

/// Undo steps 1 and 2 of [`swap_in`]. If even that fails the old copy is kept where it is
/// (the staging folder is then not deleted) and the message says where.
fn put_back(staging: &mut Staging, old: &Path, dest: &Path, moved_data: Option<PathBuf>) -> Result<(), Fail> {
    let undo = || -> io::Result<()> {
        if let Some(data) = &moved_data {
            fs::rename(data, old.join("data"))?;
        }
        rename_patiently(old, dest)
    };
    undo().map_err(|e| {
        staging.keep = true;
        log::error!("couldn't put the old plugin back from {}: {e}", old.display());
        Fail::Other(format!(
            "something went wrong and the previous version is now in {}",
            staging.root.display()
        ))
    })
}

// ---------------------------------------------------------------------------------------
// Downloading
// ---------------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Source {
    Https(String),
    File(PathBuf),
}

/// Only https goes over the network. Something that isn't an address at all is a file on
/// this computer (only the environment override can make that happen).
fn classify(source: &str) -> Result<Source, String> {
    let source = source.trim();
    let is_https = source
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
    if is_https {
        Ok(Source::Https(source.to_string()))
    } else if source.is_empty() || source.contains("://") {
        Err("only https addresses are allowed".into())
    } else {
        Ok(Source::File(PathBuf::from(source)))
    }
}

async fn fetch(source: &str, max: u64) -> Result<Vec<u8>, String> {
    match classify(source)? {
        Source::Https(url) => download(&url, max).await,
        Source::File(path) => tokio::task::spawn_blocking(move || read_local(&path, max))
            .await
            .map_err(|_| "the file could not be read".to_string())?,
    }
}

fn read_local(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| format!("can't open {} ({e})", path.display()))?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("can't read {} ({e})", path.display()))?;
    if bytes.len() as u64 > max {
        return Err("the file is too large".into());
    }
    Ok(bytes)
}

async fn download(url: &str, max: u64) -> Result<Vec<u8>, String> {
    let explain = |e: reqwest::Error| {
        log::warn!("plugin download failed: {e:?}");
        if e.is_timeout() {
            "the download took too long".to_string()
        } else if e.is_connect() {
            "no connection to the server".to_string()
        } else if e.is_redirect() {
            "the server sent the download somewhere unexpected".to_string()
        } else {
            "the network connection failed".to_string()
        }
    };
    let client = reqwest::Client::builder()
        .https_only(true) // also for every redirect on the way to the file
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(explain)?;
    let mut response = client.get(url).send().await.map_err(explain)?;
    let status = response.status();
    if !status.is_success() {
        return Err(if status == reqwest::StatusCode::NOT_FOUND {
            "there is no release to download yet".to_string()
        } else {
            format!("the server answered with error {}", status.as_u16())
        });
    }
    if response.content_length().is_some_and(|len| len > max) {
        return Err("the file is too large".into());
    }
    // Content-Length may be missing or wrong: count what actually arrives.
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(explain)? {
        if bytes.len() as u64 + chunk.len() as u64 > max {
            return Err("the file is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::proto::API_VERSION;
    use serde_json::json;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const EXE: &str = if cfg!(windows) { "xmouse-psnbridge.exe" } else { "xmouse-psnbridge" };

    /// A scratch folder per test; `plugins()` inside it is the plugins folder.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(test: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("xmouse-install-test-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("plugins")).unwrap();
            Scratch(root)
        }
        fn plugins(&self) -> PathBuf {
            self.0.join("plugins")
        }
        /// Names in the plugins folder, sorted.
        fn listing(&self) -> Vec<String> {
            names_in(&self.plugins())
        }
        /// Nothing may ever be written next to the plugins folder.
        fn assert_nothing_escaped(&self) {
            assert_eq!(names_in(&self.0), ["plugins"]);
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn manifest_json(id: &str, version: &str) -> String {
        json!({
            "id": id, "name": "PSNBridge", "version": version, "platform": "PlayStation",
            "executable": "xmouse-psnbridge", "api": API_VERSION, "license": "MIT",
            "source": "https://example.org/psnbridge"
        })
        .to_string()
    }

    /// A zip with these entries, in this order. A name ending in `/` is a folder entry.
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        zip_with(entries, CompressionMethod::Stored)
    }

    fn zip_with(entries: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
        let mut w = ZipWriter::new(io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(method);
        for (name, data) in entries {
            if name.ends_with('/') {
                w.add_directory(*name, options).unwrap();
            } else {
                w.start_file(*name, options).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    /// The writer refuses or tidies some bad names, so they are put in afterwards: the zip is
    /// written with a stand-in of the same length, which is then overwritten in the local
    /// header and in the central directory.
    fn zip_with_raw_name(raw: &str, more: &[(&str, &[u8])]) -> Vec<u8> {
        let stand_in = "Q".repeat(raw.len());
        let mut entries: Vec<(&str, &[u8])> = more.to_vec();
        entries.push((&stand_in, b"x"));
        let mut bytes = zip_of(&entries);
        let mut swapped = 0;
        let mut at = 0;
        while at + raw.len() <= bytes.len() {
            if bytes[at..at + raw.len()] == *stand_in.as_bytes() {
                bytes[at..at + raw.len()].copy_from_slice(raw.as_bytes());
                swapped += 1;
                at += raw.len();
            } else {
                at += 1;
            }
        }
        assert_eq!(swapped, 2, "{raw:?}: local header and central directory");
        bytes
    }

    fn good_files(version: &str) -> Vec<(String, Vec<u8>)> {
        vec![
            ("plugin.json".into(), manifest_json(PSNBRIDGE_ID, version).into_bytes()),
            (EXE.into(), b"program".to_vec()),
        ]
    }

    fn good_zip(version: &str, folder: bool) -> Vec<u8> {
        let files = good_files(version);
        let named: Vec<(String, &[u8])> = files
            .iter()
            .map(|(n, d)| (if folder { format!("psnbridge/{n}") } else { n.clone() }, d.as_slice()))
            .collect();
        let entries: Vec<(&str, &[u8])> = named.iter().map(|(n, d)| (n.as_str(), *d)).collect();
        zip_of(&entries)
    }

    fn install(scratch: &Scratch, zip: &[u8]) -> Result<PluginInfo, String> {
        install_zip(zip, &scratch.plugins(), PSNBRIDGE_ID)
    }

    /// A refused package leaves the plugins folder exactly as it was, with no staging folder.
    fn assert_refused(scratch: &Scratch, zip: &[u8], what: &str) -> String {
        let before = scratch.listing();
        let err = install(scratch, zip).expect_err(what);
        assert!(err.starts_with("Couldn't install PSNBridge: "), "{what}: {err}");
        assert_eq!(scratch.listing(), before, "{what}: {err}");
        scratch.assert_nothing_escaped();
        err
    }

    #[test]
    fn installs_a_zip_with_the_files_at_the_root() {
        let s = Scratch::new("root");
        let info = install(&s, &good_zip("1.0.0", false)).unwrap();
        let dir = s.plugins().join("psnbridge");
        assert_eq!((info.id.as_str(), info.version.as_str()), ("psnbridge", "1.0.0"));
        assert_eq!(info.dir, dir);
        assert_eq!(info.exe, dir.join(EXE));
        assert_eq!(info.data_dir, dir.join("data"));
        assert_eq!(fs::read(&info.exe).unwrap(), b"program");
        // What is returned is what discovery will find from now on.
        assert_eq!(manifest::load(&dir).unwrap(), info);
        assert_eq!(s.listing(), ["psnbridge"]);
        s.assert_nothing_escaped();
    }

    #[test]
    fn installs_a_zip_with_the_plugin_folder() {
        let s = Scratch::new("folder");
        let manifest = manifest_json(PSNBRIDGE_ID, "1.2.3");
        let exe = format!("psnbridge/{EXE}");
        let program = vec![7u8; 300_000];
        let zip = zip_with(
            &[
                ("psnbridge/", b""),
                ("psnbridge/plugin.json", manifest.as_bytes()),
                (&exe, &program),
                ("psnbridge/LICENSE", b"MIT"),
                ("psnbridge/README.md", b"# PSNBridge"),
            ],
            CompressionMethod::Deflated,
        );
        assert!(zip.len() < 50_000, "deflate was used");
        let info = install(&s, &zip).unwrap();
        let dir = s.plugins().join("psnbridge");
        assert_eq!(info.version, "1.2.3");
        assert_eq!(names_in(&dir), ["LICENSE", "README.md", "plugin.json", EXE]);
        assert_eq!(fs::read(&info.exe).unwrap(), program);
        assert_eq!(manifest::load(&dir).unwrap(), info);
        assert_eq!(s.listing(), ["psnbridge"]);
        // The plugins folder is created when it doesn't exist yet.
        let fresh = s.0.join("not-there-yet").join("plugins");
        install_zip(&zip, &fresh, PSNBRIDGE_ID).unwrap();
        assert_eq!(names_in(&fresh), ["psnbridge"]);
    }

    #[test]
    fn an_update_replaces_the_files_and_keeps_data() {
        let s = Scratch::new("update");
        let old = install(&s, &good_zip("1.0.0", true)).unwrap();
        fs::create_dir_all(old.data_dir.join("sub")).unwrap();
        fs::write(old.data_dir.join("pairing.json"), b"secret").unwrap();
        fs::write(old.data_dir.join("sub").join("more.bin"), b"more").unwrap();
        fs::write(old.dir.join("leftover.txt"), b"from 1.0.0").unwrap();

        let new = install(&s, &good_zip("2.0.0", false)).unwrap();
        assert_eq!(new.version, "2.0.0");
        assert_eq!(new.dir, old.dir);
        assert_eq!(fs::read(new.data_dir.join("pairing.json")).unwrap(), b"secret");
        assert_eq!(fs::read(new.data_dir.join("sub").join("more.bin")).unwrap(), b"more");
        assert_eq!(names_in(&new.dir), ["data", "plugin.json", EXE], "old files are gone");
        assert_eq!(manifest::load(&new.dir).unwrap(), new);
        assert_eq!(s.listing(), ["psnbridge"]);
        s.assert_nothing_escaped();
    }

    #[test]
    fn a_refused_update_leaves_the_installed_plugin_alone() {
        let s = Scratch::new("keep-old");
        let old = install(&s, &good_zip("1.0.0", true)).unwrap();
        fs::create_dir_all(&old.data_dir).unwrap();
        fs::write(old.data_dir.join("pairing.json"), b"secret").unwrap();

        let manifest = manifest_json(PSNBRIDGE_ID, "2.0.0");
        assert_refused(&s, &zip_of(&[("plugin.json", manifest.as_bytes())]), "no program");
        assert_refused(&s, b"this is not a zip", "not a zip");
        assert_eq!(manifest::load(&old.dir).unwrap(), old);
        assert_eq!(fs::read(old.data_dir.join("pairing.json")).unwrap(), b"secret");
    }

    #[test]
    fn names_that_point_elsewhere_are_refused() {
        let s = Scratch::new("slip");
        let files = good_files("1.0.0");
        let good: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        for bad in [
            "../evil.exe",
            "..",
            "../../evil.exe",
            "/evil.exe",
            "//server/share/evil.exe",
            "C:/evil.exe",
            "C:evil.exe",
            "C:\\evil.exe",
            "..\\evil.exe",
            "\\evil.exe",
            "sub\\evil.exe",
            "psnbridge\\evil.exe",
            "psnbridge/../evil.exe",
            "psnbridge/../../evil.exe",
            "psnbridge//evil.exe",
            "./evil.exe",
            "evil.exe:stream",
            "evil.exe.",
            "evil.exe ",
            ".hidden",
            "nul",
            "NUL.txt",
            "com1.exe",
            "evil\0.exe",
            "evil\n.exe",
            "data",
            "DATA",
            "sub/evil.exe",
            "sub/",
            "data/",
        ] {
            let err = assert_refused(&s, &zip_with_raw_name(bad, &good), bad);
            assert!(err.contains("shouldn't have") || err.contains("damaged"), "{bad:?}: {err}");
        }
        // The same in a zip that uses the plugin folder.
        let in_folder: Vec<(String, &[u8])> =
            files.iter().map(|(n, d)| (format!("psnbridge/{n}"), d.as_slice())).collect();
        let in_folder: Vec<(&str, &[u8])> = in_folder.iter().map(|(n, d)| (n.as_str(), *d)).collect();
        for bad in [
            "evil.exe", // next to the folder instead of inside it
            "../evil.exe",
            "other/evil.exe",
            "psnbridge/sub/evil.exe",
            "psnbridge/sub/",
            "psnbridge/data",
            "psnbridge/data/",
            "psnbridge/data/pairing.json",
            "Psnbridge/evil.exe",
            "psnbridge/..",
            "psnbridge/C:evil.exe",
        ] {
            assert_refused(&s, &zip_with_raw_name(bad, &in_folder), bad);
        }
        assert!(s.listing().is_empty());
    }

    #[test]
    fn links_and_repeated_names_are_refused() {
        let s = Scratch::new("links");
        let files = good_files("1.0.0");
        let good: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();

        let mut w = ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in &good {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.add_symlink("link.exe", "C:/Windows/System32/cmd.exe", SimpleFileOptions::default()).unwrap();
        let with_link = w.finish().unwrap().into_inner();
        assert_refused(&s, &with_link, "symlink");

        // Exactly the same name twice, and the same name in another case.
        let err = assert_refused(&s, &zip_with_raw_name("plugin.json", &good), "duplicate");
        assert!(err.contains("twice"), "{err}");
        let err = assert_refused(&s, &zip_with_raw_name("Plugin.JSON", &good), "duplicate by case");
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn too_many_or_too_large_entries_are_refused() {
        let s = Scratch::new("limits");
        let files = good_files("1.0.0");
        let mut names: Vec<String> = Vec::new();
        for n in 0..(LIMITS.entries - 2) {
            names.push(format!("extra{n}.txt"));
        }
        let mut entries: Vec<(&str, &[u8])> =
            files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        entries.extend(names.iter().map(|n| (n.as_str(), b"x".as_slice())));
        assert_eq!(entries.len(), LIMITS.entries);
        install(&s, &zip_of(&entries)).expect("exactly the limit is fine");
        fs::remove_dir_all(s.plugins().join("psnbridge")).unwrap();

        entries.push(("one-more.txt", b"x"));
        let err = assert_refused(&s, &zip_of(&entries), "33 entries");
        assert!(err.contains("too many"), "{err}");
        assert_refused(&s, &zip_of(&[]), "empty zip");

        // Sizes, with small limits so the test doesn't need 64 MB.
        let zip = good_zip("1.0.0", true);
        let small = |limits: Limits| install_with(&zip, &s.plugins(), PSNBRIDGE_ID, &limits);
        let roomy = Limits { zip: 1 << 20, entries: 32, entry: 1 << 20, total: 1 << 20 };
        for limits in [
            Limits { zip: 100, ..roomy },
            Limits { entry: 6, ..roomy },   // "program" is 7 bytes
            Limits { total: 100, ..roomy }, // the manifest alone is longer
        ] {
            let err = small(limits).unwrap_err();
            assert!(err.contains("too large") || err.contains("too much data"), "{err}");
            assert!(s.listing().is_empty(), "{err}");
        }
        small(roomy).unwrap();
    }

    #[test]
    fn a_package_for_another_plugin_is_refused() {
        let s = Scratch::new("wrong-id");
        let other = manifest_json("other", "1.0.0");
        let err = assert_refused(
            &s,
            &zip_of(&[("plugin.json", other.as_bytes()), (EXE, b"program")]),
            "manifest id",
        );
        assert!(err.contains("not a usable plugin"), "{err}");
        // ... also when its folder says so too.
        let exe = format!("other/{EXE}");
        assert_refused(
            &s,
            &zip_of(&[("other/plugin.json", other.as_bytes()), (&exe, b"program")]),
            "folder name",
        );
        // An id that can't be a folder name never reaches the disk.
        for id in ["", "..", "a/b", "a\\b", "C:", "Upper", ".install-x"] {
            assert!(install_zip(&good_zip("1.0.0", false), &s.plugins(), id).is_err(), "{id:?}");
        }
        assert!(s.listing().is_empty());
        s.assert_nothing_escaped();
    }

    #[test]
    fn a_package_without_program_or_manifest_is_refused() {
        let s = Scratch::new("missing");
        let manifest = manifest_json(PSNBRIDGE_ID, "1.0.0");
        let err = assert_refused(
            &s,
            &zip_of(&[("psnbridge/plugin.json", manifest.as_bytes()), ("psnbridge/README.md", b"hi")]),
            "no program",
        );
        assert!(err.contains("does not contain the plugin's program"), "{err}");
        let err = assert_refused(&s, &zip_of(&[(EXE, b"program")]), "no manifest");
        assert!(err.contains("no plugin.json"), "{err}");
        assert_refused(&s, &zip_of(&[("plugin.json", b"{ not json"), (EXE, b"program")]), "bad manifest");
        assert!(s.listing().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn a_running_plugin_is_not_replaced() {
        use std::os::windows::fs::OpenOptionsExt;
        let s = Scratch::new("running");
        let old = install(&s, &good_zip("1.0.0", true)).unwrap();
        // Windows holds a running program's file like this: nobody else may write to it.
        let held = fs::OpenOptions::new().read(true).share_mode(1).open(&old.exe).unwrap();
        let err = install(&s, &good_zip("2.0.0", true)).unwrap_err();
        assert_eq!(err, "PSNBridge is running – stop playing first");
        assert_eq!(manifest::load(&old.dir).unwrap(), old, "still version 1.0.0");
        assert_eq!(s.listing(), ["psnbridge"]);
        drop(held);
        assert_eq!(install(&s, &good_zip("2.0.0", true)).unwrap().version, "2.0.0");
        assert_eq!(s.listing(), ["psnbridge"]);
    }

    #[test]
    fn leftovers_of_an_unfinished_install_are_cleaned_up() {
        let s = Scratch::new("stale");
        // An install that stopped after moving the old plugin aside ...
        let lost = s.plugins().join(".install-aaaa");
        fs::create_dir_all(lost.join("old").join("psnbridge")).unwrap();
        fs::write(lost.join("old").join("psnbridge").join("plugin.json"), b"old").unwrap();
        fs::create_dir_all(lost.join("new").join("psnbridge")).unwrap();
        // ... and one that only left rubbish.
        fs::create_dir_all(s.plugins().join(".install-bbbb").join("new")).unwrap();

        // Discovery doesn't mistake them for broken plugins.
        let (found, skipped) = manifest::discover_in(&s.plugins());
        assert!(found.is_empty() && skipped.is_empty(), "{skipped:?}");

        // Young ones may belong to a running install and are left alone.
        sweep_stale(&s.plugins(), Duration::from_secs(3600));
        assert_eq!(s.listing(), [".install-aaaa", ".install-bbbb"]);

        sweep_stale(&s.plugins(), Duration::ZERO);
        assert_eq!(s.listing(), ["psnbridge"]);
        assert_eq!(fs::read(s.plugins().join("psnbridge").join("plugin.json")).unwrap(), b"old");
    }

    #[test]
    fn only_https_goes_over_the_network() {
        let url = "https://example.org/xmouse-psnbridge.zip";
        assert_eq!(classify(url), Ok(Source::Https(url.into())));
        assert_eq!(classify(" HTTPS://example.org/a.zip "), Ok(Source::Https("HTTPS://example.org/a.zip".into())));
        for bad in [
            "http://example.org/a.zip",
            "ftp://example.org/a.zip",
            "file:///C:/a.zip",
            "data://x",
            "https:/example.org/a.zip://",
            "",
        ] {
            assert!(classify(bad).is_err(), "{bad:?}");
        }
        assert_eq!(classify("C:\\tmp\\a.zip"), Ok(Source::File("C:\\tmp\\a.zip".into())));
        assert_eq!(classify("build/a.zip"), Ok(Source::File("build/a.zip".into())));
        assert!(PSNBRIDGE_URL.starts_with("https://github.com/"));
        assert!(matches!(classify(PSNBRIDGE_URL), Ok(Source::Https(_))));
    }

    #[test]
    fn a_local_package_is_read_with_the_same_size_limit() {
        let s = Scratch::new("local");
        let path = s.0.join("plugins").join("package.zip");
        fs::write(&path, good_zip("1.0.0", true)).unwrap();
        let bytes = read_local(&path, 1 << 20).unwrap();
        assert_eq!(bytes, good_zip("1.0.0", true));
        assert!(read_local(&path, 10).unwrap_err().contains("too large"));
        assert!(read_local(&s.0.join("nowhere.zip"), 10).is_err());
    }
}
