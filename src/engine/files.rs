use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Lowercased entry names -> is_dir for one folder. One read_dir answers
/// every sibling lookup — a stat per path crawls on some storage/AV setups
/// (~25ms each); only symlink/junction entries pay a real stat.
fn dir_entries(d: &Path) -> HashMap<String, bool> {
    fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let is_dir = e
                .file_type()
                .is_ok_and(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir()));
            (e.file_name().to_string_lossy().to_lowercase(), is_dir)
        })
        .collect()
}

/// Enumerate several folders in parallel. On network shares every read_dir
/// is a round trip (~25ms+), so scanning many folders serially adds whole
/// seconds.
fn list_dirs(dirs: Vec<PathBuf>) -> HashMap<PathBuf, HashMap<String, bool>> {
    let next = AtomicUsize::new(0);
    let out = Mutex::new(HashMap::new());
    // ponytail: fixed cap of 16 workers; tune if enormous drops ever crawl
    std::thread::scope(|s| {
        for _ in 0..dirs.len().min(16) {
            s.spawn(|| {
                while let Some(d) = dirs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let entries = dir_entries(d);
                    out.lock().unwrap().insert(d.clone(), entries);
                }
            });
        }
    });
    out.into_inner().unwrap()
}

/// Per-folder listing cache shared by the GUI (classifying dropped/listed
/// paths) and the batch planner (collision checks). Names match
/// case-insensitively, like NTFS.
pub struct FsKinds {
    map: HashMap<PathBuf, HashMap<String, bool>>,
}

impl FsKinds {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Enumerate all not-yet-cached folders in parallel up front — serial
    /// round trips to a network share add whole seconds.
    pub fn warm(&mut self, dirs: impl IntoIterator<Item = PathBuf>) {
        let dirs: Vec<PathBuf> = dirs
            .into_iter()
            .filter(|d| !d.as_os_str().is_empty() && !self.map.contains_key(d))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        self.map.extend(list_dirs(dirs));
    }

    /// Warm the parents of `paths`, ready to classify each path cheaply.
    pub fn warm_parents(&mut self, paths: &[PathBuf]) {
        self.warm(
            paths
                .iter()
                .filter_map(|p| p.parent().map(Path::to_path_buf)),
        );
    }

    fn dir(&mut self, d: &Path) -> &HashMap<String, bool> {
        self.map
            .entry(d.to_path_buf())
            .or_insert_with(|| dir_entries(d))
    }

    /// Some(true)=dir, Some(false)=file, None=missing.
    // ponytail: case-insensitive keys — a Linux folder holding both "A" and
    // "a" can misclassify; the stat fallback still covers unreadable parents.
    pub fn kind(&mut self, p: &Path) -> Option<bool> {
        let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else {
            return p.is_dir().then_some(true); // drive roots etc.
        };
        if parent.as_os_str().is_empty() {
            return fs::metadata(p).ok().map(|m| m.is_dir());
        }
        self.dir(parent)
            .get(&name.to_string_lossy().to_lowercase())
            .copied()
            .or_else(|| fs::metadata(p).ok().map(|m| m.is_dir()))
    }

    /// Whether `p` exists on disk, matching the name case-insensitively.
    pub fn exists(&mut self, p: &Path) -> bool {
        match (p.parent(), p.file_name()) {
            (Some(d), Some(n)) if !d.as_os_str().is_empty() => self
                .dir(d)
                .contains_key(&n.to_string_lossy().to_lowercase()),
            _ => p.exists(),
        }
    }
}

// ───────────────────────── sorting and globbing

// Split into (text, number) chunks so "img9" < "img10".
pub fn natural_key(s: &str) -> Vec<(String, u64)> {
    let mut key = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                key.push((text.to_lowercase(), digits.parse().unwrap_or(u64::MAX)));
                text = String::new();
                digits.clear();
            }
            text.push(c);
        }
    }
    // No trailing digits = 0 (sorts before any number); overflow = MAX,
    // matching the mid-string branch above.
    let last = if digits.is_empty() {
        0
    } else {
        digits.parse().unwrap_or(u64::MAX)
    };
    key.push((text.to_lowercase(), last));
    key
}

/// Sort by "name" (natural), "ext", "size", or "date". Returns false for any
/// other kind ("none", "manual") and leaves the order untouched.
pub fn sort_files(files: &mut [PathBuf], kind: &str) -> bool {
    match kind {
        "name" => files.sort_by_cached_key(|f| natural_key(&name_of(f))),
        "ext" => files.sort_by_cached_key(|f| super::split_ext(&name_of(f)).1.to_lowercase()),
        "size" => files.sort_by_cached_key(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0)),
        "date" => files.sort_by_cached_key(|f| fs::metadata(f).and_then(|m| m.modified()).ok()),
        _ => return false,
    }
    true
}

// PowerShell doesn't expand globs, so handle * and ? ourselves.
// `dirs` switches matching from files to folders.
pub fn expand(arg: &str, dirs: bool) -> Vec<PathBuf> {
    if !arg.contains('*') && !arg.contains('?') {
        return vec![PathBuf::from(arg)];
    }
    let p = PathBuf::from(arg);
    let re = mask_re(&name_of(&p));
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            // read_dir already knows the entry kind; only symlinks need a stat
            let kind_ok = e.file_type().is_ok_and(|t| {
                if dirs {
                    t.is_dir() || (t.is_symlink() && e.path().is_dir())
                } else {
                    t.is_file() || (t.is_symlink() && e.path().is_file())
                }
            });
            if kind_ok && re.is_match(&n) {
                out.push(if dir.as_os_str() == "." {
                    PathBuf::from(n)
                } else {
                    dir.join(n)
                });
            }
        }
    }
    if out.is_empty() {
        eprintln!("warning: '{arg}' matched nothing");
    }
    out
}

/// Wildcard mask ("*.jpg", "img?") as an anchored, case-insensitive regex.
/// (?s) so '*'/'?' also cover the '\n' Unix allows in file names.
pub(super) fn mask_re(pat: &str) -> Regex {
    let body = regex::escape(pat).replace(r"\*", ".*").replace(r"\?", ".");
    Regex::new(&format!("(?is)^{body}$")).unwrap()
}

/// Include/exclude filename masks: "*.jpg;*.png;!*thumb*".
pub struct Masks {
    inc: Vec<Regex>,
    exc: Vec<Regex>,
}

impl Masks {
    pub fn parse(s: &str) -> Masks {
        let (mut inc, mut exc) = (Vec::new(), Vec::new());
        for m in s.split(';').map(str::trim).filter(|m| !m.is_empty()) {
            match m.strip_prefix('!') {
                Some(x) => exc.push(mask_re(x)),
                None => inc.push(mask_re(m)),
            }
        }
        Masks { inc, exc }
    }

    pub fn pass(&self, name: &str) -> bool {
        (self.inc.is_empty() || self.inc.iter().any(|m| m.is_match(name)))
            && !self.exc.iter().any(|m| m.is_match(name))
    }
}

/// Collect files under `dir`, optionally recursively, honoring masks.
pub fn collect_dir(dir: &Path, recurse: bool, masks: &Masks, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else { continue };
            let p = e.path();
            // read_dir already knows the entry kind; only symlinks need a stat
            if t.is_dir() || (t.is_symlink() && p.is_dir()) {
                if recurse {
                    collect_dir(&p, true, masks, out);
                }
            } else if (t.is_file() || p.is_file()) && masks.pass(&name_of(&p)) {
                out.push(p);
            }
        }
    }
}

pub fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
