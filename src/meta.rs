// Metadata fields backed by a user-installed ExifTool (deliberately not
// bundled — see todo.md). Located via the IRON_RENAMER_EXIFTOOL env var or
// as "exiftool" on PATH. All fields of a file are read in one call and
// cached for the session.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::OnceLock;

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

fn run(tool: &Path, args: &[&OsStr]) -> Option<String> {
    let mut cmd = Command::new(tool);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash from the GUI
    }
    let out = cmd.output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

pub fn available() -> bool {
    tool().is_some()
}

/// A metadata field (case-insensitive ExifTool tag name) for a file.
/// None = ExifTool unavailable or the file unreadable; Some("") = tag absent.
pub fn get(path: &Path, tag: &str) -> Option<String> {
    Some(fields(path)?.get(&tag.to_ascii_lowercase()).cloned().unwrap_or_default())
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
    // -s2: "TagName: value" lines · -fast: skip slow scans · -m: ignore minor errors
    let out = run(t, &[OsStr::new("-s2"), OsStr::new("-fast"), OsStr::new("-m"), path.as_os_str()])?;
    let mut map = HashMap::new();
    for line in out.lines() {
        if let Some((k, v)) = line.split_once(": ") {
            map.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Some(Rc::new(map))
}
