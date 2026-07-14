// Shared batch planner/executor used by both the CLI and GUI.
// Plans validate names and collisions up front (applying the collision
// policy so the preview shows final names); execution orders chains, breaks
// swap cycles with temp names, never leaves temps behind, creates
// destination directories, and falls back to copy+delete for cross-volume
// moves. Every applied rename/move batch is recorded in a dated history
// file for selective undo (copies are not undoable and are not recorded).

use crate::engine::{Ctx, RuleEntry, apply_entry, name_of, split_ext};
use crate::tags;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Rename,
    Copy,
    Move,
}

#[derive(Clone, PartialEq)]
pub enum Collision {
    Fail,
    Number,          // "name (2).ext"
    Letter,          // "name_b.ext"
    Pattern(String), // append a tag-expanded suffix to the stem
}

pub struct BatchCfg<'a> {
    pub rules: &'a [RuleEntry],
    pub start: usize,
    pub pad: usize,
    pub overrides: &'a HashMap<PathBuf, String>,
    pub mode: Mode,
    /// Destination folder template for Copy/Move; tags expand per file.
    /// Empty = the file's own folder. Relative paths are joined to it.
    pub dest: &'a str,
    pub collision: Collision,
}

impl<'a> BatchCfg<'a> {
    #[cfg(test)] // frontends fill the full struct; tests want plain renames
    pub fn rename(
        rules: &'a [RuleEntry],
        start: usize,
        pad: usize,
        overrides: &'a HashMap<PathBuf, String>,
    ) -> Self {
        BatchCfg { rules, start, pad, overrides, mode: Mode::Rename, dest: "", collision: Collision::Fail }
    }
}

pub struct PlanItem {
    pub from: PathBuf,
    pub new_name: String,
    pub target: PathBuf,
    pub changed: bool,
    pub issue: Option<String>,
}

impl PlanItem {
    pub fn op(&self) -> Op {
        Op { from: self.from.clone(), to: self.target.clone() }
    }
}

// ───────────────────────── validation

fn reserved(name: &str) -> bool {
    // Device names are reserved with any extension: "CON", "con.txt", "LPT1.log".
    let s = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(s.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (s.len() == 4
            && (s.starts_with("COM") || s.starts_with("LPT"))
            && matches!(s.as_bytes()[3], b'1'..=b'9'))
}

pub fn name_issue(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("empty name".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (*c as u32) < 0x20)
    {
        return Some(format!("invalid character '{}'", c.escape_default()));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("ends with dot or space".into());
    }
    if reserved(name) {
        return Some("reserved Windows name".into());
    }
    None
}

// ───────────────────────── planning

fn lower_abs(p: &Path) -> String {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase()
}

/// Apply rules (or a manual override) to every file, resolve the destination,
/// and flag issues: bad names, duplicate targets, on-disk collisions, and
/// over-long paths. With a non-Fail collision policy, colliding names get a
/// suffix so the preview already shows the final result. Case-only renames
/// are valid; collision checks are case-insensitive like NTFS.
pub fn plan(files: &[PathBuf], cfg: &BatchCfg) -> Vec<PlanItem> {
    struct Pre {
        name: String,
        dest_dir: PathBuf,
        changed: bool,
    }

    // Pass 1: names and base targets.
    let mut pre: Vec<Pre> = Vec::with_capacity(files.len());
    let mut per_folder: HashMap<String, usize> = HashMap::new();
    let mut ctxs: Vec<(usize, usize)> = Vec::with_capacity(files.len()); // (num, folder_num)
    for (i, f) in files.iter().enumerate() {
        let original = name_of(f);
        let folder = f.parent().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
        let folder_num = *per_folder.entry(folder).and_modify(|n| *n += 1).or_insert(1);
        let ctx = Ctx {
            index: i,
            num: cfg.start + i,
            pad: cfg.pad,
            folder_num,
            path: f,
            original: &original,
        };
        ctxs.push((ctx.num, folder_num));
        let name = match cfg.overrides.get(f) {
            Some(o) => o.clone(),
            None => {
                let mut n = original.clone();
                for e in cfg.rules {
                    n = apply_entry(e, &n, &ctx);
                }
                n
            }
        };
        let dest_dir = if cfg.dest.is_empty() {
            f.parent().map(PathBuf::from).unwrap_or_default()
        } else {
            let expanded = PathBuf::from(tags::expand(cfg.dest, &name, &ctx));
            if expanded.is_absolute() {
                expanded
            } else {
                f.parent().map(|p| p.join(&expanded)).unwrap_or(expanded)
            }
        };
        let changed = dest_dir.join(&name) != *f;
        pre.push(Pre { name, dest_dir, changed });
    }

    // A target on disk is only a conflict if a batch item vacates that path.
    let vacates = |cand: &Path| {
        cfg.mode != Mode::Copy
            && files
                .iter()
                .zip(&pre)
                .any(|(g, p)| p.changed && lower_abs(g) == lower_abs(cand))
    };

    // Pass 2: sequential collision resolution.
    let mut taken: HashSet<String> = HashSet::new();
    let mut items: Vec<PlanItem> = Vec::with_capacity(files.len());
    for (i, (f, p)) in files.iter().zip(&pre).enumerate() {
        let original = name_of(f);
        let ctx = Ctx {
            index: i,
            num: ctxs[i].0,
            pad: cfg.pad,
            folder_num: ctxs[i].1,
            path: f,
            original: &original,
        };
        let mut name = p.name.clone();
        let mut target = p.dest_dir.join(&name);
        let mut issue = name_issue(&name);

        if p.changed && issue.is_none() {
            let self_lower = lower_abs(f);
            let mut n = 1usize;
            loop {
                let key = lower_abs(&target);
                let is_self = key == self_lower;
                // In copy mode "the same file, different case" is still a
                // collision; for rename it is a valid case-only rename.
                let disk = target.exists() && !vacates(&target) && (!is_self || cfg.mode == Mode::Copy);
                let dup = taken.contains(&key);
                if !disk && !dup {
                    break;
                }
                let suffix = match &cfg.collision {
                    Collision::Fail => {
                        issue = Some(if dup { "duplicate target".into() } else { "target exists".into() });
                        break;
                    }
                    Collision::Number => {
                        n += 1;
                        format!(" ({n})")
                    }
                    Collision::Letter => {
                        n += 1;
                        format!("_{}", alpha(n))
                    }
                    Collision::Pattern(pat) => {
                        if n > 1 {
                            issue = Some("collision pattern is not unique".into());
                            break;
                        }
                        n += 1;
                        tags::expand(pat, &p.name, &ctx)
                    }
                };
                let (stem, ext) = split_ext(&p.name);
                name = if ext.is_empty() {
                    format!("{stem}{suffix}")
                } else {
                    format!("{stem}{suffix}.{ext}")
                };
                if let Some(e) = name_issue(&name) {
                    issue = Some(e);
                    break;
                }
                target = p.dest_dir.join(&name);
            }
            if issue.is_none()
                && std::path::absolute(&target).map(|t| t.as_os_str().len()).unwrap_or(0) > 259
            {
                issue = Some("path too long".into());
            }
        }
        taken.insert(lower_abs(&target));
        items.push(PlanItem { from: f.clone(), new_name: name, target, changed: p.changed, issue });
    }
    items
}

fn alpha(mut n: usize) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

// ───────────────────────── execution

pub struct ExecResult {
    /// Successful operations in execution order, original path -> final path.
    pub renamed: Vec<Op>,
    pub failed: Vec<(Op, String)>,
}

fn transfer(from: &Path, to: &Path, mode: Mode) -> io::Result<()> {
    if let Some(dir) = to.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir)?;
    }
    match mode {
        Mode::Copy => fs::copy(from, to).map(|_| ()),
        Mode::Rename | Mode::Move => fs::rename(from, to).or_else(|e| {
            // ERROR_NOT_SAME_DEVICE (17) on Windows, EXDEV (18) on Linux.
            if mode == Mode::Move && matches!(e.raw_os_error(), Some(17) | Some(18)) {
                fs::copy(from, to)?;
                fs::remove_file(from)
            } else {
                Err(e)
            }
        }),
    }
}

/// Execute a batch safely. For rename/move: ops blocked by another pending
/// source wait their turn (chains), pure cycles (a<->b) are broken with a
/// temp name, and a temp is renamed back if its final step fails. Copies
/// never vacate sources, so they run as a simple loop. A failed op leaves
/// its file untouched so the same batch can be retried.
pub fn execute(ops: Vec<Op>, mode: Mode) -> ExecResult {
    let mut renamed = Vec::new();
    let mut failed: Vec<(Op, String)> = Vec::new();

    if mode == Mode::Copy {
        for op in ops {
            let res = if op.to.exists() {
                Err(io::Error::new(io::ErrorKind::AlreadyExists, "target exists"))
            } else {
                transfer(&op.from, &op.to, mode)
            };
            match res {
                Ok(_) => renamed.push(op),
                Err(e) => failed.push((op, e.to_string())),
            }
        }
        return ExecResult { renamed, failed };
    }

    struct P {
        orig: PathBuf,
        cur: PathBuf,
        to: PathBuf,
    }
    let low = |p: &Path| p.to_string_lossy().to_lowercase();
    let mut pending: Vec<P> =
        ops.into_iter().map(|o| P { orig: o.from.clone(), cur: o.from, to: o.to }).collect();
    let mut tmp_n = 0u32;

    while !pending.is_empty() {
        let unblocked = (0..pending.len()).find(|&i| {
            !pending.iter().enumerate().any(|(j, q)| j != i && low(&q.cur) == low(&pending[i].to))
        });
        if let Some(i) = unblocked {
            let p = pending.remove(i);
            let case_only = low(&p.cur) == low(&p.to);
            // fs::rename overwrites on Unix; refuse instead of clobbering.
            let res = if !case_only && p.to.exists() {
                Err(io::Error::new(io::ErrorKind::AlreadyExists, "target exists"))
            } else {
                transfer(&p.cur, &p.to, mode)
            };
            match res {
                Ok(_) => renamed.push(Op { from: p.orig, to: p.to }),
                Err(e) => {
                    let mut msg = e.to_string();
                    if p.cur != p.orig && fs::rename(&p.cur, &p.orig).is_err() {
                        msg = format!("{msg} (file left at temporary name '{}')", p.cur.display());
                    }
                    failed.push((Op { from: p.orig, to: p.to }, msg));
                }
            }
        } else {
            // Pure cycle: move one file aside so the others can proceed.
            let mut tmp;
            loop {
                tmp_n += 1;
                tmp = pending[0].to.with_file_name(format!(".irtmp_{}_{tmp_n}", std::process::id()));
                if !tmp.exists() {
                    break;
                }
            }
            match fs::rename(&pending[0].cur, &tmp) {
                Ok(_) => pending[0].cur = tmp,
                Err(e) => {
                    let p = pending.remove(0);
                    failed.push((Op { from: p.orig, to: p.to }, e.to_string()));
                }
            }
        }
    }
    ExecResult { renamed, failed }
}

// ───────────────────────── preview export

/// Write the preview to `path`; the extension picks the format:
/// .csv, .json, or plain text ("old -> target") for anything else.
pub fn export_preview(items: &[PlanItem], path: &Path) -> io::Result<()> {
    use crate::presets::{csv_field, json_str};
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let status = |it: &PlanItem| match (&it.issue, it.changed) {
        (Some(e), _) => e.clone(),
        (None, true) => "ok".to_string(),
        (None, false) => "unchanged".to_string(),
    };
    let body = match ext.as_str() {
        "csv" => {
            let mut s = String::from("old,new,target,status\n");
            for it in items {
                s.push_str(&format!(
                    "{},{},{},{}\n",
                    csv_field(&name_of(&it.from)),
                    csv_field(&it.new_name),
                    csv_field(&it.target.display().to_string()),
                    csv_field(&status(it)),
                ));
            }
            s
        }
        "json" => {
            let rows: Vec<String> = items
                .iter()
                .map(|it| {
                    format!(
                        "  {{\"old\": {}, \"new\": {}, \"target\": {}, \"status\": {}}}",
                        json_str(&name_of(&it.from)),
                        json_str(&it.new_name),
                        json_str(&it.target.display().to_string()),
                        json_str(&status(it)),
                    )
                })
                .collect();
            format!("[\n{}\n]\n", rows.join(",\n"))
        }
        _ => {
            let mut s = String::new();
            for it in items {
                s.push_str(&format!(
                    "{}  ->  {}   [{}]\n",
                    it.from.display(),
                    it.target.display(),
                    status(it),
                ));
            }
            s
        }
    };
    fs::write(path, body)
}

/// Write an execution result log to `path` (.csv, .json, or text).
pub fn export_results(res: &ExecResult, path: &Path) -> io::Result<()> {
    use crate::presets::{csv_field, json_str};
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let rows: Vec<(&Op, String)> = res
        .renamed
        .iter()
        .map(|op| (op, "done".to_string()))
        .chain(res.failed.iter().map(|(op, e)| (op, format!("failed: {e}"))))
        .collect();
    let body = match ext.as_str() {
        "csv" => {
            let mut s = String::from("from,to,result\n");
            for (op, r) in &rows {
                s.push_str(&format!(
                    "{},{},{}\n",
                    csv_field(&op.from.display().to_string()),
                    csv_field(&op.to.display().to_string()),
                    csv_field(r),
                ));
            }
            s
        }
        "json" => {
            let objs: Vec<String> = rows
                .iter()
                .map(|(op, r)| {
                    format!(
                        "  {{\"from\": {}, \"to\": {}, \"result\": {}}}",
                        json_str(&op.from.display().to_string()),
                        json_str(&op.to.display().to_string()),
                        json_str(r),
                    )
                })
                .collect();
            format!("[\n{}\n]\n", objs.join(",\n"))
        }
        _ => {
            let mut s = String::new();
            for (op, r) in &rows {
                s.push_str(&format!("{}  ->  {}   [{r}]\n", op.from.display(), op.to.display()));
            }
            s
        }
    };
    fs::write(path, body)
}

// ───────────────────────── history

fn history_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("iron_renamer"))
        .or_else(|| std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".iron_renamer")))
        .unwrap_or_else(|| PathBuf::from(".iron_renamer"))
        .join("history.tsv")
}

/// Append an applied batch (in execution order) to the history file.
pub fn record(ops: &[Op]) -> io::Result<u64> {
    record_at(&history_path(), ops)
}

pub(crate) fn record_at(path: &Path, ops: &[Op]) -> io::Result<u64> {
    if ops.is_empty() {
        return Ok(0);
    }
    let id = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut body = String::new();
    for op in ops {
        body.push_str(&format!("{id}\t{}\t{}\n", op.from.display(), op.to.display()));
    }
    fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(body.as_bytes())?;
    Ok(id)
}

fn read_history(path: &Path) -> Vec<(u64, Op)> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            let id = parts.next()?.parse().ok()?;
            let from = PathBuf::from(parts.next()?);
            let to = PathBuf::from(parts.next()?);
            Some((id, Op { from, to }))
        })
        .collect()
}

/// Past batches as (id, date, item count), newest first.
pub fn history() -> Vec<(u64, String, usize)> {
    history_at(&history_path())
}

fn history_at(path: &Path) -> Vec<(u64, String, usize)> {
    let mut out: Vec<(u64, String, usize)> = Vec::new();
    for (id, _) in read_history(path) {
        match out.iter_mut().find(|(i, ..)| *i == id) {
            Some((.., n)) => *n += 1,
            None => out.push((id, date_str(id), 1)),
        }
    }
    out.sort_by_key(|e| std::cmp::Reverse(e.0));
    out
}

/// Revert one batch (latest if `id` is None) through the same safe executor,
/// so undoing swaps, chains, and moves works too. Reverted entries are
/// removed from history; entries that failed to revert are kept for retry.
/// Returns the reverted ops (new path -> restored original path).
pub fn undo(id: Option<u64>) -> Result<(Vec<Op>, Vec<String>), String> {
    undo_at(&history_path(), id)
}

pub(crate) fn undo_at(path: &Path, id: Option<u64>) -> Result<(Vec<Op>, Vec<String>), String> {
    let all = read_history(path);
    let id = id
        .or_else(|| all.iter().map(|(i, _)| *i).max())
        .ok_or("no batch history")?;
    let batch: Vec<Op> = all.iter().filter(|(i, _)| *i == id).map(|(_, o)| o.clone()).collect();
    if batch.is_empty() {
        return Err(format!("no batch with id {id} (see 'history')"));
    }

    let inverse: Vec<Op> =
        batch.iter().rev().map(|o| Op { from: o.to.clone(), to: o.from.clone() }).collect();
    // Move handles everything undo needs: directory creation and volumes.
    let res = execute(inverse, Mode::Move);

    // A failed inverse op's `to` is the original `from` of the recorded op.
    let still_applied: Vec<&PathBuf> = res.failed.iter().map(|(op, _)| &op.to).collect();
    let keep: String = all
        .iter()
        .filter(|(i, o)| *i != id || still_applied.contains(&&o.from))
        .map(|(i, o)| format!("{i}\t{}\t{}\n", o.from.display(), o.to.display()))
        .collect();
    let write_res =
        if keep.is_empty() { fs::remove_file(path).or(Ok(())) } else { fs::write(path, keep) };
    if let Err(e) = write_res {
        return Err(format!("reverted {} but could not update history: {e}", res.renamed.len()));
    }

    let errors = res
        .failed
        .iter()
        .map(|(op, e)| format!("{} -> {}: {e}", op.from.display(), op.to.display()))
        .collect();
    Ok((res.renamed, errors))
}

fn date_str(id_millis: u64) -> String {
    let (y, m, d, h, mi, _) = tags::civil_utc((id_millis / 1000) as i64);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_rule;

    fn rules(specs: &[(&str, &str, &str)]) -> Vec<RuleEntry> {
        specs
            .iter()
            .map(|(kind, a, b)| {
                let (rule, part) = build_rule(kind, &[], a, b).unwrap();
                RuleEntry { rule, part, cond: None }
            })
            .collect()
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("iron_renamer_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    #[test]
    fn validates_names() {
        assert!(name_issue("ok.txt").is_none());
        assert!(name_issue("common.txt").is_none()); // COM without a digit is fine
        assert!(name_issue("").is_some());
        assert!(name_issue("a<b.txt").is_some());
        assert!(name_issue("a\tb.txt").is_some());
        assert!(name_issue("CON.txt").is_some());
        assert!(name_issue("com3").is_some());
        assert!(name_issue("trailing.").is_some());
        assert!(name_issue("trailing ").is_some());
    }

    #[test]
    fn swap_and_chain() {
        let d = tmpdir("swap");
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");
        let res = execute(
            vec![Op { from: a.clone(), to: b.clone() }, Op { from: b.clone(), to: a.clone() }],
            Mode::Rename,
        );
        assert_eq!(res.renamed.len(), 2);
        assert!(res.failed.is_empty());
        assert_eq!(read(&a), "B");
        assert_eq!(read(&b), "A");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2, "no temp files left behind");

        let d = tmpdir("chain");
        let one = put(&d, "1.txt", "one");
        let two = put(&d, "2.txt", "two");
        let three = d.join("3.txt");
        let res = execute(
            vec![Op { from: one.clone(), to: two.clone() }, Op { from: two.clone(), to: three.clone() }],
            Mode::Rename,
        );
        assert!(res.failed.is_empty());
        assert_eq!(read(&two), "one");
        assert_eq!(read(&three), "two");
        assert!(!one.exists());
    }

    #[test]
    fn partial_failure_preserves_files_for_retry() {
        let d = tmpdir("partial");
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");
        let blocker = put(&d, "taken.txt", "X");
        let res = execute(
            vec![
                Op { from: a.clone(), to: d.join("taken.txt") },
                Op { from: b.clone(), to: d.join("free.txt") },
            ],
            Mode::Rename,
        );
        assert_eq!(res.renamed.len(), 1);
        assert_eq!(res.failed.len(), 1);
        assert_eq!(read(&a), "A", "failed op leaves its file untouched");
        assert_eq!(read(&blocker), "X", "existing file never overwritten");
        assert_eq!(read(&d.join("free.txt")), "B");
    }

    #[test]
    fn copy_and_move_modes() {
        let d = tmpdir("copymove");
        let a = put(&d, "a.txt", "A");
        let sub = d.join("out").join("deep");

        // Copy into a subfolder that does not exist yet.
        let res = execute(vec![Op { from: a.clone(), to: sub.join("a.txt") }], Mode::Copy);
        assert!(res.failed.is_empty());
        assert_eq!(read(&a), "A", "copy keeps the source");
        assert_eq!(read(&sub.join("a.txt")), "A");

        // Copy refuses to overwrite.
        let res = execute(vec![Op { from: a.clone(), to: sub.join("a.txt") }], Mode::Copy);
        assert_eq!(res.failed.len(), 1);

        // Move creates directories and removes the source.
        let b = put(&d, "b.txt", "B");
        let res = execute(vec![Op { from: b.clone(), to: sub.join("b.txt") }], Mode::Move);
        assert!(res.failed.is_empty());
        assert!(!b.exists());
        assert_eq!(read(&sub.join("b.txt")), "B");
    }

    #[test]
    fn plan_flags_conflicts_and_allows_case_only() {
        let d = tmpdir("plan");
        put(&d, "img1.jpg", "");
        put(&d, "img2.jpg", "");
        put(&d, "other.jpg", "");
        let files = vec![d.join("img1.jpg"), d.join("img2.jpg")];
        let none = HashMap::new();

        let case_rule = rules(&[("replace", "img", "IMG")]);
        let items = plan(&files, &BatchCfg::rename(&case_rule, 1, 1, &none));
        assert!(items.iter().all(|i| i.changed && i.issue.is_none()), "case-only renames are valid");

        let dup_rule = rules(&[("pattern", "same.jpg", "")]);
        let items = plan(&files, &BatchCfg::rename(&dup_rule, 1, 1, &none));
        assert_eq!(items[1].issue.as_deref(), Some("duplicate target"));

        let clash_rule = rules(&[("replace", "img1", "other")]);
        let items = plan(&files, &BatchCfg::rename(&clash_rule, 1, 1, &none));
        assert_eq!(items[0].issue.as_deref(), Some("target exists"));
        assert!(items[1].issue.is_none());

        // Swap inside one batch is not a conflict: each target is vacated.
        let swap_rule = rules(&[
            ("replace", "img1", "tmpX"),
            ("replace", "img2", "img1"),
            ("replace", "tmpX", "img2"),
        ]);
        let items = plan(&files, &BatchCfg::rename(&swap_rule, 1, 1, &none));
        assert!(items.iter().all(|i| i.changed && i.issue.is_none()));

        // A manual override wins over rules but is validated like any name.
        let over: HashMap<PathBuf, String> = [(files[0].clone(), "manual.jpg".to_string())].into();
        let cfg = BatchCfg { overrides: &over, ..BatchCfg::rename(&case_rule, 1, 1, &none) };
        let items = plan(&files, &cfg);
        assert_eq!(items[0].new_name, "manual.jpg");
        assert!(items[0].issue.is_none());
    }

    #[test]
    fn collision_policies_resolve_in_preview() {
        let d = tmpdir("collide");
        put(&d, "a.jpg", "");
        put(&d, "b.jpg", "");
        put(&d, "same.jpg", "");
        let files = vec![d.join("a.jpg"), d.join("b.jpg")];
        let none = HashMap::new();
        let dup_rule = rules(&[("pattern", "same.jpg", "")]);

        let cfg = BatchCfg {
            collision: Collision::Number,
            ..BatchCfg::rename(&dup_rule, 1, 1, &none)
        };
        let items = plan(&files, &cfg);
        assert_eq!(items[0].new_name, "same (2).jpg", "disk collision numbered");
        assert_eq!(items[1].new_name, "same (3).jpg", "batch duplicate numbered");
        assert!(items.iter().all(|i| i.issue.is_none()));

        let cfg = BatchCfg {
            collision: Collision::Letter,
            ..BatchCfg::rename(&dup_rule, 1, 1, &none)
        };
        let items = plan(&files, &cfg);
        assert_eq!(items[0].new_name, "same_b.jpg");
        assert_eq!(items[1].new_name, "same_c.jpg");

        let cfg = BatchCfg {
            collision: Collision::Pattern("_<index>".into()),
            ..BatchCfg::rename(&dup_rule, 1, 1, &none)
        };
        let items = plan(&files, &cfg);
        assert_eq!(items[0].new_name, "same_1.jpg");
        assert_eq!(items[1].new_name, "same_2.jpg");
    }

    #[test]
    fn plan_copy_move_destinations() {
        let d = tmpdir("dest");
        put(&d, "a.jpg", "");
        put(&d, "b.txt", "");
        let files = vec![d.join("a.jpg"), d.join("b.txt")];
        let none = HashMap::new();

        // Tag-expanded relative destination: sorted/<ext>.
        let cfg = BatchCfg {
            mode: Mode::Copy,
            dest: "sorted\\<ext>",
            ..BatchCfg::rename(&[], 1, 1, &none)
        };
        let items = plan(&files, &cfg);
        assert!(items.iter().all(|i| i.changed && i.issue.is_none()));
        assert_eq!(items[0].target, d.join("sorted").join("jpg").join("a.jpg"));
        assert_eq!(items[1].target, d.join("sorted").join("txt").join("b.txt"));

        // Copy onto itself (empty dest, no rules) is a no-op, not a conflict.
        let cfg = BatchCfg { mode: Mode::Copy, ..BatchCfg::rename(&[], 1, 1, &none) };
        let items = plan(&files, &cfg);
        assert!(items.iter().all(|i| !i.changed));
    }

    #[test]
    fn history_records_and_selectively_undoes() {
        let d = tmpdir("hist");
        let hist = d.join("history.tsv");
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");

        // Batch: swap a and b, then undo it through history.
        let res = execute(
            vec![Op { from: a.clone(), to: b.clone() }, Op { from: b.clone(), to: a.clone() }],
            Mode::Rename,
        );
        assert!(res.failed.is_empty());
        let id = record_at(&hist, &res.renamed).unwrap();
        assert_eq!(history_at(&hist), vec![(id, date_str(id), 2)]);

        let (reverted, errors) = undo_at(&hist, Some(id)).unwrap();
        assert_eq!(reverted.len(), 2);
        assert!(errors.is_empty());
        assert_eq!(read(&a), "A");
        assert_eq!(read(&b), "B");
        assert!(history_at(&hist).is_empty(), "fully undone batch is removed from history");

        // A move batch undoes back out of its subfolder.
        let res = execute(vec![Op { from: a.clone(), to: d.join("sub").join("a.txt") }], Mode::Move);
        assert!(res.failed.is_empty());
        record_at(&hist, &res.renamed).unwrap();
        let (reverted, errors) = undo_at(&hist, None).unwrap();
        assert_eq!((reverted.len(), errors.len()), (1, 0));
        assert_eq!(read(&a), "A");
    }

    #[test]
    fn failed_undo_entries_stay_in_history() {
        let d = tmpdir("histfail");
        let hist = d.join("history.tsv");
        let a = put(&d, "a.txt", "A");
        let renamed_a = d.join("a2.txt");
        let b = put(&d, "b.txt", "B");
        let renamed_b = d.join("b2.txt");
        let res = execute(
            vec![
                Op { from: a.clone(), to: renamed_a.clone() },
                Op { from: b.clone(), to: renamed_b.clone() },
            ],
            Mode::Rename,
        );
        record_at(&hist, &res.renamed).unwrap();

        // Occupy a's original name so undoing it must fail.
        put(&d, "a.txt", "squatter");
        let (reverted, errors) = undo_at(&hist, None).unwrap();
        assert_eq!(reverted.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(read(&b), "B");
        assert_eq!(read(&renamed_a), "A", "failed revert leaves the file where it was");
        assert_eq!(history_at(&hist).len(), 1, "failed entry kept for retry");

        // Clear the squatter and retry the same batch id.
        fs::remove_file(&a).unwrap();
        let (reverted, errors) = undo_at(&hist, None).unwrap();
        assert_eq!((reverted.len(), errors.len()), (1, 0));
        assert_eq!(read(&a), "A");
        assert!(history_at(&hist).is_empty());
    }
}
