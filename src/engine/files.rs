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

// These live here rather than in engine/tests.rs because the warming tests
// assert on FsKinds::map, which is private to this module. Warming is a pure
// performance optimisation -- deleting it leaves every result correct and
// merely restores the per-file round trips that 0.3.3/0.3.4 removed -- so the
// only thing that can catch a regression is asserting the cache is populated.
#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("iron_files_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn warming_lists_each_folder_once_up_front() {
        let root = tmpdir("warm");
        let (one, two) = (root.join("one"), root.join("two"));
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        fs::write(one.join("a.txt"), "").unwrap();
        fs::write(two.join("b.txt"), "").unwrap();
        fs::create_dir(two.join("nested")).unwrap();

        let paths = vec![one.join("a.txt"), one.join("gone.txt"), two.join("b.txt")];
        let mut k = FsKinds::new();
        k.warm_parents(&paths);

        // Both parents cached after warming, before any per-path lookup.
        assert_eq!(
            k.map.len(),
            2,
            "cached: {:?}",
            k.map.keys().collect::<Vec<_>>()
        );
        assert_eq!(k.map[&one].len(), 1);
        assert!(!k.map[&two]["b.txt"]);
        assert!(k.map[&two]["nested"]);

        // Warming again must not re-list a folder that is already cached.
        k.warm(vec![one.clone(), two.clone()]);
        assert_eq!(k.map.len(), 2);

        // Entries are matched case-insensitively, like NTFS.
        assert_eq!(k.kind(&one.join("A.TXT")), Some(false));
        assert_eq!(k.kind(&two.join("NESTED")), Some(true));
        assert!(k.exists(&two.join("B.txt")));
        assert!(!k.exists(&one.join("gone.txt")));
    }

    #[test]
    fn warming_skips_paths_with_no_real_parent() {
        let mut k = FsKinds::new();
        // A bare name's parent is "", which is not a folder to enumerate.
        k.warm_parents(&[PathBuf::from("bare.txt")]);
        assert!(
            k.map.is_empty(),
            "cached: {:?}",
            k.map.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn natural_key_splits_into_text_and_number_runs() {
        // The exact shape matters: a stray empty chunk still sorts plausibly
        // in most orderings, so only the structure catches it.
        assert_eq!(natural_key("img10"), vec![("img".to_string(), 10)]);
        assert_eq!(natural_key("a1b2"), vec![("a".into(), 1), ("b".into(), 2)]);
        // No trailing digits sorts as 0, before any numbered sibling.
        assert_eq!(natural_key("a"), vec![("a".to_string(), 0)]);
        assert_eq!(natural_key("IMG"), vec![("img".to_string(), 0)]);
    }

    #[test]
    fn sort_files_orders_by_modification_date() {
        use crate::batch::{apply_touch, parse_touch};
        let d = tmpdir("date");
        for n in ["a.txt", "b.txt", "c.txt"] {
            fs::write(d.join(n), "").unwrap();
        }
        // Dates deliberately disagree with name order, so a "date" sort that
        // silently did nothing would leave the files in the wrong sequence.
        for (n, when) in [
            ("a.txt", "modified=2010-03-04 05:06"),
            ("b.txt", "modified=2001-03-04 05:06"),
            ("c.txt", "modified=2020-03-04 05:06"),
        ] {
            let spec = parse_touch(when).unwrap();
            let (done, errs) = apply_touch(&[d.join(n)], &spec);
            assert_eq!((done, errs.len()), (1, 0), "{errs:?}");
        }

        let orig = vec![d.join("a.txt"), d.join("b.txt"), d.join("c.txt")];
        let mut f = orig.clone();
        assert!(sort_files(&mut f, "date"));
        assert_eq!(f, vec![d.join("b.txt"), d.join("a.txt"), d.join("c.txt")]);

        // And the unknown-kind contract still holds alongside it.
        let mut f = orig.clone();
        assert!(!sort_files(&mut f, "manual"));
        assert_eq!(f, orig);
    }

    #[test]
    fn exists_answers_from_the_warmed_cache_not_a_fresh_stat() {
        let d = tmpdir("cached");
        let p = d.join("here.txt");
        fs::write(&p, "").unwrap();
        let mut k = FsKinds::new();
        k.warm_parents(std::slice::from_ref(&p));
        assert!(k.exists(&p));

        // Deleting the file behind the cache must not change the answer: a
        // per-call stat here is exactly the round trip warming exists to avoid.
        fs::remove_file(&p).unwrap();
        assert!(
            k.exists(&p),
            "exists() re-stat'ed instead of using the cache"
        );
        assert!(!p.exists());
    }

    // Windows refuses file symlinks without Developer Mode, but a directory
    // junction needs no privilege and Rust reports it as a symlink, which is
    // what the "is it a folder" branches actually test. A junction whose
    // target is gone is the discriminating case: still a link, no longer a
    // folder. WSL is not installed here; the file-symlink halves of these
    // branches, and the #[cfg(unix)] code, need a Linux run.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    #[cfg(windows)]
    fn a_broken_directory_link_is_not_a_folder() {
        let root = tmpdir("junction");
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("inner.txt"), "").unwrap();

        let live = root.join("live");
        let dead = root.join("dead");
        let gone = root.join("gone");
        fs::create_dir_all(&gone).unwrap();
        if !junction(&live, &real) || !junction(&dead, &gone) {
            return; // no junction support: nothing to assert
        }
        fs::remove_dir(&gone).unwrap(); // now `dead` points at nothing

        // A live link resolves to a folder; a dangling one must not, or the
        // planner would treat a broken reparse point as a directory.
        let mut k = FsKinds::new();
        k.warm(vec![root.clone()]);
        assert_eq!(k.kind(&live), Some(true), "live junction is a folder");
        assert_eq!(k.kind(&dead), Some(false), "dead junction is not a folder");

        // Globbing for folders includes the live link and skips the dead one.
        let pat = root.join("*").to_string_lossy().into_owned();
        let mut dirs = expand(&pat, true);
        dirs.sort();
        assert_eq!(dirs, vec![live.clone(), real.clone()], "dead link matched");
    }

    // The other half of the same branches: Unix gives us symlinks to files and
    // dangling links, which Windows junctions cannot express. Run under WSL.
    #[test]
    #[cfg(unix)]
    fn symlinks_are_classified_by_what_they_point_at() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("symlink");
        let realdir = root.join("realdir");
        fs::create_dir_all(&realdir).unwrap();
        fs::write(realdir.join("inner.txt"), "").unwrap();
        let realfile = root.join("real.txt");
        fs::write(&realfile, "x").unwrap();

        let to_dir = root.join("to_dir");
        let to_file = root.join("to_file.txt");
        let dangling = root.join("dangling.txt");
        symlink(&realdir, &to_dir).unwrap();
        symlink(&realfile, &to_file).unwrap();
        symlink(root.join("never_existed"), &dangling).unwrap();

        // Folder-ness follows the link, so a link to a file is not a folder
        // and a link to nothing is not one either.
        let mut k = FsKinds::new();
        k.warm(vec![root.clone()]);
        assert_eq!(k.kind(&to_dir), Some(true));
        assert_eq!(k.kind(&to_file), Some(false));
        assert_eq!(k.kind(&dangling), Some(false));

        let pat = root.join("*").to_string_lossy().into_owned();

        // Globbing for folders takes only what resolves to one.
        let mut dirs = expand(&pat, true);
        dirs.sort();
        assert_eq!(dirs, vec![realdir.clone(), to_dir.clone()]);

        // Globbing for files takes the link to a file but not the dangling one.
        let mut files = expand(&pat, false);
        files.sort();
        assert_eq!(files, vec![realfile.clone(), to_file.clone()]);

        // Collecting treats a link to a file as a file rather than descending
        // into it, and leaves a dangling link out entirely.
        let all = Masks::parse("");
        let mut flat = Vec::new();
        collect_dir(&root, false, &all, &mut flat);
        flat.sort();
        assert_eq!(flat, vec![realfile.clone(), to_file.clone()]);
    }
}
