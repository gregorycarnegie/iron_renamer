// Shared batch planner/executor used by both the CLI and GUI.
// Plans validate names and collisions up front; execution orders chains,
// breaks swap cycles with temp names, and never leaves temps behind.
// Every applied batch is recorded in a dated history file for selective undo.

use crate::engine::{Ctx, RuleEntry, apply_entry, name_of};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub from: PathBuf,
    pub to: PathBuf,
}

pub struct PlanItem {
    pub from: PathBuf,
    pub new_name: String,
    pub changed: bool,
    pub issue: Option<String>,
}

impl PlanItem {
    pub fn op(&self) -> Op {
        Op { from: self.from.clone(), to: self.from.with_file_name(&self.new_name) }
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

/// Apply rules to every file and flag issues: bad names, in-batch duplicate
/// targets, on-disk collisions, and over-long paths. Case-only renames are
/// valid (NTFS handles them); collision checks are case-insensitive like NTFS.
/// A manual override replaces the rule result for that file but is validated
/// the same way.
pub fn plan(
    files: &[PathBuf],
    rules: &[RuleEntry],
    start: usize,
    pad: usize,
    overrides: &HashMap<PathBuf, String>,
) -> Vec<PlanItem> {
    let lower = |s: &str| s.to_lowercase();
    let mut names: Vec<String> = Vec::with_capacity(files.len());
    let mut per_folder: HashMap<String, usize> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        let original = name_of(f);
        let folder = f.parent().map(|p| lower(&p.to_string_lossy())).unwrap_or_default();
        let folder_num = per_folder.entry(folder).and_modify(|n| *n += 1).or_insert(1);
        let ctx =
            Ctx { index: i, num: start + i, pad, folder_num: *folder_num, path: f, original: &original };
        let mut name = match overrides.get(f) {
            Some(o) => o.clone(),
            None => original.clone(),
        };
        if !overrides.contains_key(f) {
            for e in rules {
                name = apply_entry(e, &name, &ctx);
            }
        }
        names.push(name);
    }

    files
        .iter()
        .zip(&names)
        .enumerate()
        .map(|(i, (f, name))| {
            let changed = *name != name_of(f);
            let mut issue = None;
            if changed {
                issue = name_issue(name);
                let target = f.with_file_name(name.as_str());
                let case_only = lower(name) == lower(&name_of(f));
                if issue.is_none() {
                    let dup = names.iter().enumerate().any(|(j, n)| {
                        j != i && lower(n) == lower(name) && files[j].parent() == f.parent()
                    });
                    // A target on disk is only a conflict if no batch item vacates that name.
                    let vacated = || {
                        files.iter().zip(&names).any(|(g, gn)| {
                            lower(&name_of(g)) == lower(name)
                                && g.parent() == f.parent()
                                && *gn != name_of(g)
                        })
                    };
                    if dup {
                        issue = Some("duplicate target".into());
                    } else if !case_only && target.exists() && !vacated() {
                        issue = Some("target exists".into());
                    } else if std::path::absolute(&target).map(|p| p.as_os_str().len()).unwrap_or(0) > 259 {
                        issue = Some("path too long".into());
                    }
                }
            }
            PlanItem { from: f.clone(), new_name: name.clone(), changed, issue }
        })
        .collect()
}

// ───────────────────────── execution

pub struct ExecResult {
    /// Successful renames in execution order, original path -> final path.
    pub renamed: Vec<Op>,
    pub failed: Vec<(Op, String)>,
}

/// Rename a batch safely: ops blocked by another pending source wait their
/// turn (chains), pure cycles (a<->b) are broken with a temp name, and a temp
/// is renamed back if its final step fails. A failed op leaves its file
/// untouched so the same batch can be retried.
pub fn execute(ops: Vec<Op>) -> ExecResult {
    struct P {
        orig: PathBuf,
        cur: PathBuf,
        to: PathBuf,
    }
    let low = |p: &Path| p.to_string_lossy().to_lowercase();
    let mut pending: Vec<P> =
        ops.into_iter().map(|o| P { orig: o.from.clone(), cur: o.from, to: o.to }).collect();
    let mut renamed = Vec::new();
    let mut failed: Vec<(Op, String)> = Vec::new();
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
                fs::rename(&p.cur, &p.to)
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

fn record_at(path: &Path, ops: &[Op]) -> io::Result<u64> {
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
/// so undoing swaps and chains works too. Reverted entries are removed from
/// history; entries that failed to revert are kept for retry.
/// Returns the reverted ops (new path -> restored original path).
pub fn undo(id: Option<u64>) -> Result<(Vec<Op>, Vec<String>), String> {
    undo_at(&history_path(), id)
}

fn undo_at(path: &Path, id: Option<u64>) -> Result<(Vec<Op>, Vec<String>), String> {
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
    let res = execute(inverse);

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
    let (y, m, d, h, mi, _) = crate::tags::civil_utc((id_millis / 1000) as i64);
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
        let res = execute(vec![
            Op { from: a.clone(), to: b.clone() },
            Op { from: b.clone(), to: a.clone() },
        ]);
        assert_eq!(res.renamed.len(), 2);
        assert!(res.failed.is_empty());
        assert_eq!(read(&a), "B");
        assert_eq!(read(&b), "A");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2, "no temp files left behind");

        let d = tmpdir("chain");
        let one = put(&d, "1.txt", "one");
        let two = put(&d, "2.txt", "two");
        let three = d.join("3.txt");
        let res = execute(vec![
            Op { from: one.clone(), to: two.clone() },
            Op { from: two.clone(), to: three.clone() },
        ]);
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
        let res = execute(vec![
            Op { from: a.clone(), to: d.join("taken.txt") },
            Op { from: b.clone(), to: d.join("free.txt") },
        ]);
        assert_eq!(res.renamed.len(), 1);
        assert_eq!(res.failed.len(), 1);
        assert_eq!(read(&a), "A", "failed op leaves its file untouched");
        assert_eq!(read(&blocker), "X", "existing file never overwritten");
        assert_eq!(read(&d.join("free.txt")), "B");
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
        let items = plan(&files, &case_rule, 1, 1, &none);
        assert!(items.iter().all(|i| i.changed && i.issue.is_none()), "case-only renames are valid");

        let dup_rule = rules(&[("pattern", "same.jpg", "")]);
        let items = plan(&files, &dup_rule, 1, 1, &none);
        assert!(items.iter().all(|i| i.issue.as_deref() == Some("duplicate target")));

        let clash_rule = rules(&[("replace", "img1", "other")]);
        let items = plan(&files, &clash_rule, 1, 1, &none);
        assert_eq!(items[0].issue.as_deref(), Some("target exists"));
        assert!(items[1].issue.is_none());

        // Swap inside one batch is not a conflict: each target is vacated.
        let swap_rule = rules(&[
            ("replace", "img1", "tmpX"),
            ("replace", "img2", "img1"),
            ("replace", "tmpX", "img2"),
        ]);
        let items = plan(&files, &swap_rule, 1, 1, &none);
        assert!(items.iter().all(|i| i.changed && i.issue.is_none()));

        // A manual override wins over rules but is validated like any name.
        let over: HashMap<PathBuf, String> =
            [(files[0].clone(), "manual.jpg".to_string())].into();
        let items = plan(&files, &case_rule, 1, 1, &over);
        assert_eq!(items[0].new_name, "manual.jpg");
        assert!(items[0].issue.is_none());
        let bad: HashMap<PathBuf, String> = [(files[0].clone(), "CON.jpg".to_string())].into();
        let items = plan(&files, &case_rule, 1, 1, &bad);
        assert_eq!(items[0].issue.as_deref(), Some("reserved Windows name"));
    }

    #[test]
    fn history_records_and_selectively_undoes() {
        let d = tmpdir("hist");
        let hist = d.join("history.tsv");
        let a = put(&d, "a.txt", "A");
        let b = put(&d, "b.txt", "B");

        // Batch: swap a and b, then undo it through history.
        let res = execute(vec![
            Op { from: a.clone(), to: b.clone() },
            Op { from: b.clone(), to: a.clone() },
        ]);
        assert!(res.failed.is_empty());
        let id = record_at(&hist, &res.renamed).unwrap();
        assert_eq!(history_at(&hist), vec![(id, date_str(id), 2)]);

        let (reverted, errors) = undo_at(&hist, Some(id)).unwrap();
        assert_eq!(reverted.len(), 2);
        assert!(errors.is_empty());
        assert_eq!(read(&a), "A");
        assert_eq!(read(&b), "B");
        assert!(history_at(&hist).is_empty(), "fully undone batch is removed from history");
    }

    #[test]
    fn failed_undo_entries_stay_in_history() {
        let d = tmpdir("histfail");
        let hist = d.join("history.tsv");
        let a = put(&d, "a.txt", "A");
        let renamed_a = d.join("a2.txt");
        let b = put(&d, "b.txt", "B");
        let renamed_b = d.join("b2.txt");
        let res = execute(vec![
            Op { from: a.clone(), to: renamed_a.clone() },
            Op { from: b.clone(), to: renamed_b.clone() },
        ]);
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
