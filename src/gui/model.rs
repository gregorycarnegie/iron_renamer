use crate::{
    batch::Op,
    engine::{FsKinds, RuleEntry, build_rule, name_of, natural_key},
};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
};

#[derive(Clone)]
pub(super) struct RuleSpec {
    pub(super) kind: String,
    pub(super) a: String,
    pub(super) b: String,
    pub(super) mods: String, // colon-separated, same syntax as the CLI flag suffixes
}

impl RuleSpec {
    pub(super) fn build(&self) -> Result<RuleEntry, String> {
        let mods: Vec<&str> = self.mods.split(':').filter(|m| !m.is_empty()).collect();
        let (rule, part) = build_rule(&self.kind, &mods, &self.a, &self.b)?;
        Ok(RuleEntry {
            rule,
            part,
            cond: None,
        })
    }

    pub(super) fn summary(&self) -> String {
        let b = |default: &str| {
            if self.b.is_empty() {
                default.to_string()
            } else {
                self.b.clone()
            }
        };
        let mut s = match self.kind.as_str() {
            "replace" => format!("\"{}\" → \"{}\"", self.a, self.b),
            "regex" => format!("/{}/ → \"{}\"", self.a, self.b),
            "case" if !self.b.is_empty() => format!("{} @ {}", self.a, self.b),
            "insert" => format!("\"{}\" @ {}", self.a, b("end")),
            "move" => format!("\"{}\" → {}", self.a, b("end")),
            "renumber" => format!("#{} {}", self.a, self.b),
            "swap" => format!("around \"{}\"", self.a),
            "names" => format!("{} name(s)", self.a.lines().count()),
            "pairs" => format!("{} pair(s)", self.a.lines().count()),
            "js" => format!("script · {} line(s)", self.a.lines().count()),
            "trim" if self.a.is_empty() => "whitespace".into(),
            _ => self.a.clone(),
        };
        if !self.mods.is_empty() {
            s = format!("{s} [{}]", self.mods);
        }
        s
    }
}

#[derive(Default)]
pub(super) struct State {
    pub(super) files: Vec<PathBuf>,
    pub(super) rules: Vec<RuleSpec>,
    pub(super) overrides: HashMap<PathBuf, String>, // per-item manual new names
    pub(super) dirs: bool,                          // files or folders, never mixed
    pub(super) can_undo: bool,
    pub(super) editing: Option<usize>,
    pub(super) sel: BTreeSet<usize>, // multi-selected row indices
    pub(super) anchor: usize,        // shift-click range anchor
}

// New items arrive natural-sorted among themselves but never disturb the
// existing order, so manual reordering sticks.
pub(super) fn add_files(s: &mut State, mut paths: Vec<PathBuf>) {
    paths.sort_by_cached_key(|p| natural_key(&name_of(p)));
    for p in paths {
        if !s.files.contains(&p) {
            s.files.push(p);
        }
    }
}

// Point list entries at their post-batch locations; consumed (or stale,
// after undo) overrides go with them.
pub(super) fn retarget(s: &mut State, ops: &[Op]) {
    for op in ops {
        if let Some(f) = s.files.iter_mut().find(|f| **f == op.from) {
            *f = op.to.clone();
        }
        s.overrides.remove(&op.from);
    }
}

pub(super) fn remove_rule(s: &mut State, i: usize) {
    if i < s.rules.len() {
        s.rules.remove(i);
        s.editing = match s.editing {
            Some(e) if e == i => None,
            Some(e) if e > i => Some(e - 1),
            other => other,
        };
    }
}

pub(super) fn move_rule(s: &mut State, from: usize, to: isize) {
    if from < s.rules.len() && to >= 0 && (to as usize) < s.rules.len() {
        let to = to as usize;
        s.rules.swap(from, to);
        s.editing = match s.editing {
            Some(e) if e == from => Some(to),
            Some(e) if e == to => Some(from),
            other => other,
        };
    }
}

// A saved list is one path per line; folders-only lists reload in folder
// mode. Missing paths (or files in a folder list) are skipped and counted.
pub(super) fn parse_list(body: &str) -> (Vec<PathBuf>, bool, usize) {
    let paths: Vec<PathBuf> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    let total = paths.len();
    let mut kinds = FsKinds::new();
    kinds.warm_parents(&paths);
    let dirs_mode = !paths.is_empty() && paths.iter().all(|p| kinds.kind(p) == Some(true));
    let want = Some(dirs_mode);
    let keep: Vec<PathBuf> = paths
        .into_iter()
        .filter(|p| kinds.kind(p) == want)
        .collect();
    let skipped = total - keep.len();
    (keep, dirs_mode, skipped)
}

pub(super) fn parse_csv_import(body: &str) -> (Vec<PathBuf>, HashMap<PathBuf, String>, usize) {
    let mut files = Vec::new();
    let mut overrides = HashMap::new();
    let mut skipped = 0;
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let cols = crate::presets::csv_split(line);
        let path = PathBuf::from(cols[0].trim());
        if !path.is_file() {
            skipped += 1;
            continue;
        }
        if let Some(new) = cols.get(1).map(|c| c.trim()).filter(|c| !c.is_empty()) {
            overrides.insert(path.clone(), new.to_string());
        }
        files.push(path);
    }
    (files, overrides, skipped)
}

pub(super) fn specs_from_preset(
    rules: Vec<(String, String, String, String)>,
) -> (Vec<RuleSpec>, usize) {
    let mut specs = Vec::new();
    let mut bad = 0;
    for (kind, mods, a, b) in rules {
        let spec = RuleSpec { kind, a, b, mods };
        if spec.build().is_ok() {
            specs.push(spec);
        } else {
            bad += 1;
        }
    }
    (specs, bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn gui_state_adds_naturally_sorted_unique_files() {
        let mut state = State {
            files: vec!["kept.txt".into()],
            ..State::default()
        };
        add_files(
            &mut state,
            ["item10.txt", "item2.txt", "kept.txt"]
                .map(PathBuf::from)
                .to_vec(),
        );
        assert_eq!(
            state.files,
            ["kept.txt", "item2.txt", "item10.txt"].map(PathBuf::from)
        );
    }

    #[test]
    fn gui_rule_spec_builds_cli_compatible_modifiers() {
        let spec = RuleSpec {
            kind: "replace".into(),
            a: "old".into(),
            b: "new".into(),
            mods: "name:ci:first".into(),
        };
        assert!(spec.build().is_ok());
        assert_eq!(spec.summary(), "\"old\" → \"new\" [name:ci:first]");
    }

    fn spec(kind: &str, a: &str, b: &str) -> RuleSpec {
        RuleSpec {
            kind: kind.into(),
            a: a.into(),
            b: b.into(),
            mods: String::new(),
        }
    }

    #[test]
    fn gui_rule_summaries_per_kind() {
        for (s, expected) in [
            (spec("regex", r"\d+", "n"), r#"/\d+/ → "n""#),
            (spec("insert", "x", ""), "\"x\" @ end"),
            (spec("names", "a\nb\nc", ""), "3 name(s)"),
            (spec("trim", "", ""), "whitespace"),
            (spec("case", "upper", ""), "upper"),
        ] {
            assert_eq!(s.summary(), expected);
        }
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("iron_renamer_gui_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn list_load_keeps_existing_files_and_detects_folder_mode() {
        let d = tmpdir("list");
        let f = d.join("a.txt");
        fs::write(&f, "x").unwrap();
        let body = format!("{}\n\n{}\n", f.display(), d.join("missing.txt").display());
        let (keep, dirs_mode, skipped) = parse_list(&body);
        assert_eq!((keep, dirs_mode, skipped), (vec![f], false, 1));
        let (keep, dirs_mode, skipped) = parse_list(&format!("{}\n", d.display()));
        assert_eq!((keep, dirs_mode, skipped), (vec![d], true, 0));
    }

    #[test]
    fn csv_import_reads_overrides_and_skips_header() {
        let d = tmpdir("csv");
        let f = d.join("a.txt");
        fs::write(&f, "x").unwrap();
        let body = format!("path,new name\n{},renamed.txt\n", f.display());
        let (files, overrides, skipped) = parse_csv_import(&body);
        assert_eq!((files, skipped), (vec![f.clone()], 1));
        assert_eq!(overrides[&f], "renamed.txt");
    }

    #[test]
    fn preset_rules_load_and_bad_ones_are_counted() {
        let (specs, bad) = specs_from_preset(vec![
            ("replace".into(), "ci".into(), "a".into(), "b".into()),
            ("regex".into(), String::new(), "[".into(), String::new()),
        ]);
        assert_eq!((specs.len(), bad), (1, 1));
        assert_eq!(specs[0].kind, "replace");
    }

    #[test]
    fn retarget_follows_renames_and_drops_consumed_overrides() {
        let mut state = State {
            files: vec!["a.txt".into(), "b.txt".into()],
            overrides: HashMap::from([("a.txt".into(), "manual".into())]),
            ..State::default()
        };
        retarget(
            &mut state,
            &[Op {
                from: "a.txt".into(),
                to: "z.txt".into(),
            }],
        );
        assert_eq!(state.files, ["z.txt", "b.txt"].map(PathBuf::from));
        assert!(state.overrides.is_empty());
    }

    #[test]
    fn rule_stack_edit_highlight_follows_remove_and_move() {
        let mut state = State {
            rules: vec![
                spec("case", "upper", ""),
                spec("trim", "", ""),
                spec("swap", "-", ""),
            ],
            editing: Some(1),
            ..State::default()
        };
        remove_rule(&mut state, 0);
        assert_eq!((state.rules.len(), state.editing), (2, Some(0)));
        move_rule(&mut state, 0, 1);
        assert_eq!(state.editing, Some(1));
        assert_eq!(state.rules[1].kind, "trim");
        move_rule(&mut state, 0, 5);
        assert_eq!(state.editing, Some(1));
        remove_rule(&mut state, 1);
        assert_eq!((state.rules.len(), state.editing), (1, None));
    }
}
