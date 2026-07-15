// Metadata fields backed by a user-installed ExifTool (deliberately not
// bundled — see todo.md). Located via the IRON_RENAMER_EXIFTOOL env var or
// as "exiftool" on PATH. All fields of a file are read in one call and
// cached for the session.

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::OnceLock,
};

fn tool() -> Option<&'static PathBuf> {
    static TOOL: OnceLock<Option<PathBuf>> = OnceLock::new();
    TOOL.get_or_init(|| {
        let cand = std::env::var_os("IRON_RENAMER_EXIFTOOL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("exiftool"));
        run(&cand, &[OsStr::new("-ver")]).map(|_| cand)
    })
    .as_ref()
}

fn cmd(tool: &Path) -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut c = Command::new(tool);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash from the GUI
    }
    c
}

fn run(tool: &Path, args: &[&OsStr]) -> Option<String> {
    let out = cmd(tool).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

#[cfg(test)] // probe for the gated test; frontends surface missing ExifTool via literal tags
pub fn available() -> bool {
    tool().is_some()
}

/// A metadata field (case-insensitive ExifTool tag name) for a file.
/// None = ExifTool unavailable or the file unreadable; Some("") = tag absent.
pub fn get(path: &Path, tag: &str) -> Option<String> {
    Some(
        fields(path)?
            .get(&tag.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default(),
    )
}

/// Write metadata tags ("TAG=VALUE" each) on files in one ExifTool call.
/// Returns ExifTool's summary line (e.g. "2 image files updated").
pub fn set(paths: &[PathBuf], assigns: &[String]) -> Result<String, String> {
    let t = tool().ok_or("ExifTool not found (install it or set IRON_RENAMER_EXIFTOOL)")?;
    let mut c = cmd(t);
    c.arg("-overwrite_original").arg("-m");
    for a in assigns {
        c.arg(format!("-{a}"));
    }
    c.args(paths);
    let out = c.output().map_err(|e| e.to_string())?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() {
        Ok(text(&out.stdout))
    } else {
        Err(text(&out.stderr))
    }
}

type Fields = Rc<HashMap<String, String>>;

fn fields(path: &Path) -> Option<Fields> {
    thread_local! {
        static CACHE: RefCell<HashMap<PathBuf, Option<Fields>>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        if let Some(hit) = c.borrow().get(path) {
            return hit.clone();
        }
        let val = read_fields(path);
        c.borrow_mut().insert(path.to_path_buf(), val.clone());
        val
    })
}

fn read_fields(path: &Path) -> Option<Fields> {
    let t = tool()?;
    // -s2: "TagName: value" lines · -fast: skip slow scans · -m: ignore minor
    // errors · -c: GPS coordinates as signed decimal (file-name friendly)
    let out = run(
        t,
        &[
            OsStr::new("-s2"),
            OsStr::new("-fast"),
            OsStr::new("-m"),
            OsStr::new("-c"),
            OsStr::new("%+.6f"),
            path.as_os_str(),
        ],
    )?;
    let mut map = HashMap::new();
    for line in out.lines() {
        if let Some((k, v)) = line.split_once(": ") {
            map.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Some(Rc::new(map))
}
