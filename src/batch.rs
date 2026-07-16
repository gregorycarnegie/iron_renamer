// Shared batch planner/executor used by both the CLI and GUI.
// Plans validate names and collisions up front; execution orders chains,
// breaks swap cycles safely, and supports copy and cross-volume moves.

use crate::{
    engine::{Ctx, RuleEntry, apply_entry, join_ext, name_of, split_ext},
    tags,
};
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

mod export;
mod history;
mod touch;

#[cfg(test)]
use export::export_rows;
pub use export::{export_preview, export_results};
#[cfg(test)]
use history::{date_str, history_at};
pub use history::{history, record, undo};
#[cfg(test)]
pub(crate) use history::{record_at, undo_at};
pub use touch::{apply_touch, parse_touch};

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

impl Collision {
    /// "fail" | "number" | "letter" | "pattern" (suffix from `pattern`,
    /// default "_<num>"); anything else is an inline pattern.
    pub fn parse(policy: &str, pattern: &str) -> Collision {
        match policy {
            "fail" => Collision::Fail,
            "number" => Collision::Number,
            "letter" => Collision::Letter,
            "pattern" => Collision::Pattern(if pattern.is_empty() {
                "_<num>".into()
            } else {
                pattern.into()
            }),
            p => Collision::Pattern(p.to_string()),
        }
    }
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
    /// File-pair mode: items sharing a folder and stem (img1.jpg + img1.xmp)
    /// take the new stem of the first of them and keep their own extension.
    pub pairs: bool,
    /// Rows for the `<csv:COL>` tag, loaded by the frontend (`--csv FILE`).
    pub csv: &'a [Vec<String>],
}

impl<'a> BatchCfg<'a> {
    #[cfg(test)] // frontends fill the full struct; tests want plain renames
    pub fn rename(
        rules: &'a [RuleEntry],
        start: usize,
        pad: usize,
        overrides: &'a HashMap<PathBuf, String>,
    ) -> Self {
        BatchCfg {
            rules,
            start,
            pad,
            overrides,
            mode: Mode::Rename,
            dest: "",
            collision: Collision::Fail,
            pairs: false,
            csv: &[],
        }
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
        Op {
            from: self.from.clone(),
            to: self.target.clone(),
        }
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

/// Characters Windows forbids in file names (shared with tag sanitizing).
pub(crate) const INVALID_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

pub fn name_issue(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("empty name".into());
    }
    if let Some(c) = name
        .chars()
        .find(|c| INVALID_CHARS.contains(c) || (*c as u32) < 0x20)
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
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}

/// Apply rules (or a manual override) to every file, resolve the destination,
/// and flag issues: bad names, duplicate targets, on-disk collisions, and
/// over-long paths. With a non-Fail collision policy, colliding names get a
/// suffix so the preview already shows the final result. Case-only renames
/// are valid; collision checks are case-insensitive like NTFS.
pub fn plan(files: &[PathBuf], cfg: &BatchCfg) -> Vec<PlanItem> {
    crate::engine::reset_js(); // JS rule state never leaks between previews/batches
    struct Pre {
        name: String,
        dest_dir: PathBuf,
        changed: bool,
    }

    // Pass 1: names and base targets.
    let mut pre: Vec<Pre> = Vec::with_capacity(files.len());
    let mut per_folder: HashMap<String, usize> = HashMap::new();
    let mut pair_primary: HashMap<(String, String), usize> = HashMap::new();
    let mut folder_nums: Vec<usize> = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        let original = name_of(f);
        let folder = f
            .parent()
            .map(|p| p.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let folder_num = *per_folder
            .entry(folder.clone())
            .and_modify(|n| *n += 1)
            .or_insert(1);
        let ctx = Ctx {
            index: i,
            num: cfg.start + i,
            pad: cfg.pad,
            folder_num,
            total: files.len(),
            csv: cfg.csv,
            path: f,
            original: &original,
        };
        folder_nums.push(folder_num);
        let mut name = match cfg.overrides.get(f) {
            Some(o) => o.clone(),
            None => {
                let mut n = original.clone();
                for e in cfg.rules {
                    n = apply_entry(e, &n, &ctx);
                }
                n
            }
        };
        // File-pair mode: later members of a (folder, stem) group adopt the
        // first member's new stem; explicit overrides win.
        if cfg.pairs && !cfg.overrides.contains_key(f) {
            let key = (folder.clone(), split_ext(&original).0.to_lowercase());
            match pair_primary.get(&key) {
                Some(&j) => {
                    name = join_ext(split_ext(&pre[j].name).0, split_ext(&original).1);
                }
                None => {
                    pair_primary.insert(key, i);
                }
            }
        }
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
        pre.push(Pre {
            name,
            dest_dir,
            changed,
        });
    }

    // A target on disk is only a conflict if no batch item vacates that path.
    let vacated: HashSet<String> = if cfg.mode == Mode::Copy {
        HashSet::new()
    } else {
        files
            .iter()
            .zip(&pre)
            .filter(|(_, p)| p.changed)
            .map(|(f, _)| lower_abs(f))
            .collect()
    };

    // Pass 2: sequential collision resolution.
    let mut taken: HashSet<String> = HashSet::new();
    let mut items: Vec<PlanItem> = Vec::with_capacity(files.len());
    for (i, (f, p)) in files.iter().zip(&pre).enumerate() {
        let original = name_of(f);
        let ctx = Ctx {
            index: i,
            num: cfg.start + i,
            pad: cfg.pad,
            folder_num: folder_nums[i],
            total: files.len(),
            csv: cfg.csv,
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
                let disk = target.exists()
                    && !vacated.contains(&key)
                    && (!is_self || cfg.mode == Mode::Copy);
                let dup = taken.contains(&key);
                if !disk && !dup {
                    break;
                }
                let suffix = match &cfg.collision {
                    Collision::Fail => {
                        issue = Some(if dup {
                            "duplicate target".into()
                        } else {
                            "target exists".into()
                        });
                        break;
                    }
                    Collision::Number => {
                        n += 1;
                        format!(" ({n})")
                    }
                    Collision::Letter => {
                        n += 1;
                        format!("_{}", tags::alpha(n as i64))
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
                name = join_ext(&format!("{stem}{suffix}"), ext);
                if let Some(e) = name_issue(&name) {
                    issue = Some(e);
                    break;
                }
                target = p.dest_dir.join(&name);
            }
            if issue.is_none()
                && std::path::absolute(&target)
                    .map(|t| t.as_os_str().len())
                    .unwrap_or(0)
                    > 259
            {
                issue = Some("path too long".into());
            }
        }
        taken.insert(lower_abs(&target));
        items.push(PlanItem {
            from: f.clone(),
            new_name: name,
            target,
            changed: p.changed,
            issue,
        });
    }
    items
}

// ───────────────────────── execution

pub struct ExecResult {
    /// Successful operations in execution order, original path -> final path.
    pub renamed: Vec<Op>,
    pub failed: Vec<(Op, String)>,
}

/// Final on-disk path of every planned item: the op's target where it
/// succeeded, the original path otherwise (unchanged, conflicted, or failed).
pub fn finals(items: &[PlanItem], res: &ExecResult) -> Vec<PathBuf> {
    items
        .iter()
        .map(|it| {
            res.renamed
                .iter()
                .find(|op| op.from == it.from)
                .map(|op| op.to.clone())
                .unwrap_or_else(|| it.from.clone())
        })
        .collect()
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
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "target exists",
                ))
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
        cur_key: String,
        to_key: String,
    }
    let low = |p: &Path| p.to_string_lossy().to_lowercase();
    let remove_source = |sources: &mut HashMap<String, usize>, key: &str| {
        let last = sources.get(key) == Some(&1);
        if last {
            sources.remove(key);
        } else if let Some(n) = sources.get_mut(key) {
            *n -= 1;
        }
    };
    let mut pending: Vec<P> = ops
        .into_iter()
        .map(|o| {
            let cur_key = low(&o.from);
            let to_key = low(&o.to);
            P {
                orig: o.from.clone(),
                cur: o.from,
                to: o.to,
                cur_key,
                to_key,
            }
        })
        .collect();
    let mut sources: HashMap<String, usize> = HashMap::new();
    for p in &pending {
        *sources.entry(p.cur_key.clone()).or_default() += 1;
    }
    let mut tmp_n = 0u32;

    while !pending.is_empty() {
        // ponytail: long dependency chains still scan; use a ready queue if that becomes measurable.
        let unblocked = pending.iter().position(|p| {
            sources.get(&p.to_key).copied().unwrap_or(0) <= usize::from(p.cur_key == p.to_key)
        });
        if let Some(i) = unblocked {
            let p = pending.swap_remove(i);
            remove_source(&mut sources, &p.cur_key);
            let case_only = p.cur_key == p.to_key;
            // fs::rename overwrites on Unix; refuse instead of clobbering.
            let res = if !case_only && p.to.exists() {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "target exists",
                ))
            } else {
                transfer(&p.cur, &p.to, mode)
            };
            match res {
                Ok(_) => renamed.push(Op {
                    from: p.orig,
                    to: p.to,
                }),
                Err(e) => {
                    let mut msg = e.to_string();
                    if p.cur != p.orig && fs::rename(&p.cur, &p.orig).is_err() {
                        msg = format!("{msg} (file left at temporary name '{}')", p.cur.display());
                    }
                    failed.push((
                        Op {
                            from: p.orig,
                            to: p.to,
                        },
                        msg,
                    ));
                }
            }
        } else {
            // Pure cycle: move one file aside so the others can proceed.
            let mut tmp;
            loop {
                tmp_n += 1;
                tmp = pending[0]
                    .to
                    .with_file_name(format!(".irtmp_{}_{tmp_n}", std::process::id()));
                if !tmp.exists() {
                    break;
                }
            }
            match fs::rename(&pending[0].cur, &tmp) {
                Ok(_) => {
                    let old_key = std::mem::replace(&mut pending[0].cur_key, low(&tmp));
                    remove_source(&mut sources, &old_key);
                    *sources.entry(pending[0].cur_key.clone()).or_default() += 1;
                    pending[0].cur = tmp;
                }
                Err(e) => {
                    let p = pending.swap_remove(0);
                    remove_source(&mut sources, &p.cur_key);
                    failed.push((
                        Op {
                            from: p.orig,
                            to: p.to,
                        },
                        e.to_string(),
                    ));
                }
            }
        }
    }
    ExecResult { renamed, failed }
}

#[cfg(test)]
mod tests;
