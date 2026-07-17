use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Entry names (and whether each is a directory) for several folders,
/// enumerated in parallel. On network shares every read_dir is a round trip
/// (~25ms+), so scanning many folders serially adds whole seconds.
pub fn list_dirs(dirs: Vec<PathBuf>) -> HashMap<PathBuf, Vec<(OsString, bool)>> {
    let next = AtomicUsize::new(0);
    let out = Mutex::new(HashMap::new());
    // ponytail: fixed cap of 16 workers; tune if enormous drops ever crawl
    std::thread::scope(|s| {
        for _ in 0..dirs.len().min(16) {
            s.spawn(|| {
                while let Some(d) = dirs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let entries: Vec<(OsString, bool)> = fs::read_dir(d)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| {
                            // only symlink/junction entries pay a real stat
                            let is_dir = e
                                .file_type()
                                .is_ok_and(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir()));
                            (e.file_name(), is_dir)
                        })
                        .collect();
                    out.lock().unwrap().insert(d.clone(), entries);
                }
            });
        }
    });
    out.into_inner().unwrap()
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
    let pat = name_of(&p);
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
            if kind_ok && wild_match(&pat.to_lowercase(), &n.to_lowercase()) {
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

/// Include/exclude filename masks: "*.jpg;*.png;!*thumb*".
pub struct Masks {
    inc: Vec<String>,
    exc: Vec<String>,
}

impl Masks {
    pub fn parse(s: &str) -> Masks {
        let (mut inc, mut exc) = (Vec::new(), Vec::new());
        for m in s.split(';').map(str::trim).filter(|m| !m.is_empty()) {
            match m.strip_prefix('!') {
                Some(x) => exc.push(x.to_lowercase()),
                None => inc.push(m.to_lowercase()),
            }
        }
        Masks { inc, exc }
    }

    pub fn pass(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        (self.inc.is_empty() || self.inc.iter().any(|m| wild_match(m, &n)))
            && !self.exc.iter().any(|m| wild_match(m, &n))
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

pub fn wild_match(pat: &str, s: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    go(&p, &t)
}

pub fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
