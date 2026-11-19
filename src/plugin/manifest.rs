//! Finding installed plugins: `<config dir>\plugins\<id>\plugin.json`.
//!
//! A manifest is written by whoever made the plugin, so it is checked like any other outside
//! input: sizes are bounded, the executable must stay inside the plugin's own folder, and a
//! manifest that fails any check is skipped (with the reason logged), never half-used.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use super::proto::API_VERSION;

/// Manifests are a few hundred bytes; anything bigger is not a manifest.
const MAX_MANIFEST: u64 = 64 * 1024;
/// Stop looking after this many folders, so a huge `plugins` directory can't stall start-up.
const MAX_PLUGINS: usize = 64;

/// `plugin.json` as written on disk.
#[derive(Debug, Deserialize)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    /// What the consoles are called in the UI, e.g. "PlayStation".
    platform: String,
    /// The program to start, relative to the plugin folder.
    executable: String,
    /// Extra command-line arguments (after `--xmouse-plugin`).
    #[serde(default)]
    args: Vec<String>,
    api: u32,
    #[serde(default)]
    license: String,
    /// Where the plugin's source code lives.
    #[serde(default)]
    source: String,
}

/// A plugin that passed validation and can be started.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub platform: String,
    pub license: String,
    pub source: String,
    /// The plugin's folder (also its working directory).
    pub dir: PathBuf,
    /// Full path of the program to start.
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// A folder for the plugin's own files, passed to it in `hello`.
    pub data_dir: PathBuf,
}

/// Where plugins are installed: `plugins` inside XMouse's config folder.
pub fn plugins_dir() -> PathBuf {
    crate::storage::Storage::default_dir().join("plugins")
}

/// The usable plugins in the default folder, sorted by name. Problems are logged.
pub fn discover() -> Vec<PluginInfo> {
    discover_in(&plugins_dir()).0
}

/// The usable plugins in `dir`, plus every folder that was skipped and why (for a UI that
/// wants to say "this plugin could not be loaded").
pub fn discover_in(dir: &Path) -> (Vec<PluginInfo>, Vec<(PathBuf, String)>) {
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (found, skipped); // no plugins folder: nothing installed
    };
    for entry in entries.flatten().take(MAX_PLUGINS) {
        let path = entry.path();
        // Folders starting with a dot are not plugins (an id can't start with one): the
        // installer keeps its half-finished work in `.install-…` (see `install`).
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if hidden || !path.is_dir() {
            continue;
        }
        match load(&path) {
            Ok(info) => found.push(info),
            Err(reason) => {
                log::warn!("plugin in {} skipped: {reason}", path.display());
                skipped.push((path, reason));
            }
        }
    }
    found.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    (found, skipped)
}

/// Read and check the manifest in one plugin folder.
pub fn load(dir: &Path) -> Result<PluginInfo, String> {
    let file = dir.join("plugin.json");
    let size = std::fs::metadata(&file)
        .map_err(|_| "no plugin.json".to_string())?
        .len();
    if size > MAX_MANIFEST {
        return Err(format!("plugin.json is too large ({size} bytes)"));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| format!("can't read plugin.json: {e}"))?;
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let (m, rel) = validate(&text, folder)?;

    // The path is relative and has no `..`; also make sure a link inside the folder doesn't
    // lead out of it, by comparing the real locations.
    let mut exe = dir.join(&rel);
    if cfg!(windows) && !exe.is_file() && exe.extension().is_none() {
        exe.set_extension("exe"); // lets one manifest serve Windows and other systems
    }
    if !exe.is_file() {
        return Err(format!("executable not found: {}", exe.display()));
    }
    let real_dir = dir.canonicalize().map_err(|e| format!("can't resolve the folder: {e}"))?;
    let real_exe = exe.canonicalize().map_err(|e| format!("can't resolve the executable: {e}"))?;
    if !real_exe.starts_with(&real_dir) {
        return Err("executable is outside the plugin folder".into());
    }
    Ok(PluginInfo {
        data_dir: dir.join("data"),
        dir: dir.to_path_buf(),
        exe,
        id: m.id,
        name: m.name,
        version: m.version,
        platform: m.platform,
        license: m.license,
        source: m.source,
        args: m.args,
    })
}

fn short(what: &str, s: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.chars().count() > max || s.chars().any(char::is_control) {
        return Err(format!("{what} is too long or has control characters"));
    }
    Ok(())
}

/// Everything that can be checked without touching the disk. Returns the manifest and the
/// executable's path relative to the plugin folder.
fn validate(text: &str, folder: &str) -> Result<(Manifest, PathBuf), String> {
    let m: Manifest =
        serde_json::from_str(text).map_err(|e| format!("plugin.json is not valid: {e}"))?;
    if m.api != API_VERSION {
        return Err(format!(
            "made for plugin API {} (this XMouse speaks API {API_VERSION})",
            m.api
        ));
    }
    // The id names the folder, so it has to be a plain, portable file name.
    let id_ok = !m.id.is_empty()
        && m.id.len() <= 64
        && m.id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !id_ok {
        return Err("id must be 1-64 characters of a-z, 0-9, '-' or '_'".into());
    }
    if m.id != folder {
        return Err(format!("id \"{}\" does not match its folder name", m.id));
    }
    short("name", &m.name, 80)?;
    short("version", &m.version, 40)?;
    short("platform", &m.platform, 40)?;
    if !m.license.is_empty() {
        short("license", &m.license, 80)?;
    }
    if !m.source.is_empty() {
        short("source", &m.source, 300)?;
        // It may be shown as a link: only ordinary web addresses.
        if !(m.source.starts_with("https://") || m.source.starts_with("http://")) {
            return Err("source must be an http(s) address".into());
        }
    }
    if m.args.len() > 16 || m.args.iter().any(|a| a.len() > 256 || a.contains('\0')) {
        return Err("too many or too long args".into());
    }
    short("executable", &m.executable, 260)?;
    // Windows also accepts '/' as a separator, so one spelling works everywhere; a backslash
    // is a separator only on Windows and is refused elsewhere to keep manifests portable.
    if m.executable.contains(':') || (!cfg!(windows) && m.executable.contains('\\')) {
        return Err("executable must be a plain relative path".into());
    }
    let rel = PathBuf::from(&m.executable);
    let inside = rel.components().all(|c| matches!(c, Component::Normal(_)))
        && rel.components().next().is_some();
    if !inside {
        return Err("executable must be a relative path inside the plugin folder".into());
    }
    Ok((m, rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn manifest() -> Value {
        json!({
            "id": "demo", "name": "Demo", "version": "0.1.0", "platform": "Test",
            "executable": "bin/demo", "api": API_VERSION, "license": "MIT",
            "source": "https://example.org/demo"
        })
    }

    fn check(change: impl FnOnce(&mut Value)) -> Result<(Manifest, PathBuf), String> {
        let mut m = manifest();
        change(&mut m);
        validate(&m.to_string(), "demo")
    }

    #[test]
    fn a_good_manifest_passes() {
        let (m, rel) = check(|_| {}).unwrap();
        assert_eq!((m.id.as_str(), m.platform.as_str()), ("demo", "Test"));
        assert_eq!(rel, PathBuf::from("bin/demo"));
        assert!(m.args.is_empty());
        // Optional fields may be left out.
        check(|m| {
            m.as_object_mut().unwrap().remove("license");
            m.as_object_mut().unwrap().remove("source");
        })
        .unwrap();
    }

    #[test]
    fn executable_may_not_leave_the_plugin_folder() {
        let mut bad = vec![
            "../demo",
            "bin/../../demo",
            "..",
            "/usr/bin/demo",
            "./demo",
            "",
            "C:demo",
            "C:/Windows/System32/cmd.exe",
            "demo:stream",
        ];
        if cfg!(windows) {
            bad.extend(["C:\\Windows\\System32\\cmd.exe", "\\\\server\\share\\demo.exe", "..\\demo.exe", "\\demo.exe"]);
        } else {
            bad.push("bin\\demo");
        }
        for exe in bad {
            let r = check(|m| m["executable"] = json!(exe));
            assert!(r.is_err(), "{exe:?} was accepted");
        }
        check(|m| m["executable"] = json!("demo.exe")).unwrap();
    }

    #[test]
    fn other_api_versions_are_refused() {
        for api in [0, API_VERSION + 1, 999] {
            let e = check(|m| m["api"] = json!(api)).unwrap_err();
            assert!(e.contains("API"), "{e}");
        }
        assert!(check(|m| m["api"] = json!("1")).is_err(), "api must be a number");
        assert!(check(|m| { m.as_object_mut().unwrap().remove("api"); }).is_err());
    }

    #[test]
    fn ids_names_and_links_are_checked() {
        assert!(check(|m| m["id"] = json!("other")).is_err(), "id must match the folder");
        assert!(validate(&{ let mut m = manifest(); m["id"] = json!("De mo"); m.to_string() }, "De mo").is_err());
        assert!(validate(&{ let mut m = manifest(); m["id"] = json!(".."); m.to_string() }, "..").is_err());
        assert!(check(|m| m["name"] = json!("")).is_err());
        assert!(check(|m| m["name"] = json!("x".repeat(500))).is_err());
        assert!(check(|m| m["name"] = json!("two\nlines")).is_err());
        assert!(check(|m| m["source"] = json!("file:///C:/Windows")).is_err());
        assert!(check(|m| m["source"] = json!("javascript:alert(1)")).is_err());
        assert!(check(|m| m["args"] = json!(vec!["x"; 40])).is_err());
        assert!(validate("{ not json", "demo").is_err());
        assert!(validate("[]", "demo").is_err());
    }

    #[test]
    fn discovery_skips_broken_folders_and_keeps_good_ones() {
        let root = std::env::temp_dir().join(format!("xmouse-manifest-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let plugins = root.join("plugins");
        // good: manifest + a file standing in for the program
        let good = plugins.join("demo");
        std::fs::create_dir_all(good.join("bin")).unwrap();
        let exe_name = if cfg!(windows) { "demo.exe" } else { "demo" };
        std::fs::write(good.join("bin").join(exe_name), b"x").unwrap();
        std::fs::write(good.join("plugin.json"), manifest().to_string()).unwrap();
        // the program is missing
        let missing = plugins.join("missing");
        std::fs::create_dir_all(&missing).unwrap();
        let mut m = manifest();
        m["id"] = json!("missing");
        std::fs::write(missing.join("plugin.json"), m.to_string()).unwrap();
        // no manifest at all, and one that isn't JSON
        std::fs::create_dir_all(plugins.join("empty")).unwrap();
        std::fs::create_dir_all(plugins.join("broken")).unwrap();
        std::fs::write(plugins.join("broken").join("plugin.json"), "{").unwrap();
        // a stray file next to the folders is ignored
        std::fs::write(plugins.join("readme.txt"), "hi").unwrap();

        let (found, skipped) = discover_in(&plugins);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, "demo");
        assert!(found[0].exe.is_file());
        assert_eq!(found[0].data_dir, good.join("data"));
        assert_eq!(skipped.len(), 3, "{skipped:?}");
        assert!(skipped.iter().any(|(p, why)| p.ends_with("missing") && why.contains("not found")));
        // No plugins folder is simply "none installed".
        assert!(discover_in(&root.join("nowhere")).0.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
