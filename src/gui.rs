// Slint GUI front-end over the shared engine. Live preview on every change.

use crate::{
    batch::{self, BatchCfg, Collision, Mode, Op},
    engine::{Masks, RuleEntry, build_rule, collect_dir, name_of, natural_key, split_ext},
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, collections::HashMap, fs, path::PathBuf, rc::Rc};

slint::include_modules!();

#[derive(Clone)]
struct RuleSpec {
    kind: String,
    a: String,
    b: String,
    mods: String, // colon-separated, same syntax as the CLI flag suffixes
}

impl RuleSpec {
    fn build(&self) -> Result<RuleEntry, String> {
        let mods: Vec<&str> = self.mods.split(':').filter(|m| !m.is_empty()).collect();
        let (rule, part) = build_rule(&self.kind, &mods, &self.a, &self.b)?;
        Ok(RuleEntry {
            rule,
            part,
            cond: None,
        })
    }

    fn summary(&self) -> String {
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
struct State {
    files: Vec<PathBuf>,
    rules: Vec<RuleSpec>,
    overrides: HashMap<PathBuf, String>, // per-item manual new names
    dirs: bool,                          // list holds folders, not files — never mixed
    can_undo: bool,
}

// One batch never mixes files and folders.
fn mode_blocked(ui: &MainWindow, s: &State, want_dirs: bool) -> bool {
    if !s.files.is_empty() && s.dirs != want_dirs {
        ui.set_status_text(
            if s.dirs {
                "list holds folders — Clear it before adding files"
            } else {
                "list holds files — Clear it before adding folders"
            }
            .into(),
        );
        return true;
    }
    false
}

// New items arrive natural-sorted among themselves but never disturb the
// existing order, so manual reordering sticks.
fn add_files(s: &mut State, mut paths: Vec<PathBuf>) {
    paths.sort_by_cached_key(|p| natural_key(&name_of(p)));
    for p in paths {
        if !s.files.contains(&p) {
            s.files.push(p);
        }
    }
}

// OS drag-and-drop: files add as files; a folder adds its contents with the
// current mask/recurse settings, unless the list is already in folder mode.
fn handle_drop(ui: &MainWindow, st: &Rc<RefCell<State>>, path: PathBuf) {
    if path.is_dir() {
        let folder_mode = { st.borrow().dirs && !st.borrow().files.is_empty() };
        if folder_mode {
            add_files(&mut st.borrow_mut(), vec![path]);
        } else {
            if mode_blocked(ui, &st.borrow(), false) {
                return;
            }
            let masks = Masks::parse(&ui.get_mask_text());
            let mut found = Vec::new();
            collect_dir(&path, ui.get_recurse(), &masks, &mut found);
            let mut s = st.borrow_mut();
            s.dirs = false;
            add_files(&mut s, found);
        }
    } else {
        if mode_blocked(ui, &st.borrow(), false) {
            return;
        }
        let mut s = st.borrow_mut();
        s.dirs = false;
        add_files(&mut s, vec![path]);
    }
    refresh(ui, &st.borrow());
}

// Load a preset file into the rule list and settings (Load-preset button,
// initial .preset argument, or .preset file association).
fn apply_preset(ui: &MainWindow, st: &Rc<RefCell<State>>, p: &std::path::Path) {
    match crate::presets::load(p) {
        Ok(preset) => {
            let mut specs = Vec::new();
            let mut bad = 0;
            for (kind, mods, a, b) in preset.rules {
                let spec = RuleSpec { kind, a, b, mods };
                if spec.build().is_ok() {
                    specs.push(spec);
                } else {
                    bad += 1;
                }
            }
            let n = specs.len();
            st.borrow_mut().rules = specs;
            let get = |k: &str| preset.settings.get(k).cloned().unwrap_or_default();
            let or = |v: String, d: &str| if v.is_empty() { d.to_string() } else { v };
            ui.set_start_text(or(get("start"), "1").into());
            ui.set_pad_text(get("pad").into());
            ui.set_batch_mode(or(get("mode"), "rename").into());
            ui.set_dest_text(get("dest").into());
            ui.set_collide(or(get("collide"), "fail").into());
            ui.set_collide_pattern(get("collide_pattern").into());
            refresh(ui, &st.borrow());
            ui.set_status_text(
                match bad {
                    0 => format!("loaded {n} rule(s) from preset"),
                    _ => format!("loaded {n} rule(s) from preset, {bad} invalid"),
                }
                .into(),
            );
        }
        Err(e) => ui.set_status_text(e.into()),
    }
}

/// `initial` paths (from the Explorer context menu or `iron_renamer gui`)
/// load as if dropped on the window; a .preset file loads as a preset.
pub fn run(initial: Vec<PathBuf>) -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let state = Rc::new(RefCell::new(State::default()));
    state.borrow_mut().can_undo = !batch::history().is_empty();

    macro_rules! on {
        ($setter:ident, |$ui:ident, $st:ident $(, $arg:ident : $ty:ty)*| $body:block) => {{
            let weak = ui.as_weak();
            let state = state.clone();
            ui.$setter(move |$($arg: $ty),*| {
                let $ui = weak.unwrap();
                let $st = &state;
                $body
            });
        }};
    }

    // OS file drop (winit backend).
    {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        let weak = ui.as_weak();
        let st = state.clone();
        ui.window().on_winit_window_event(move |_, ev| {
            if let WindowEvent::DroppedFile(path) = ev
                && let Some(ui) = weak.upgrade()
            {
                handle_drop(&ui, &st, path.clone());
            }
            EventResult::Propagate
        });
    }

    on!(on_pick_files, |ui, st| {
        if mode_blocked(&ui, &st.borrow(), false) {
            return;
        }
        if let Some(paths) = rfd::FileDialog::new().pick_files() {
            let mut s = st.borrow_mut();
            s.dirs = false;
            add_files(&mut s, paths);
            drop(s);
            refresh(&ui, &st.borrow());
        }
    });

    on!(on_pick_folder, |ui, st| {
        if mode_blocked(&ui, &st.borrow(), false) {
            return;
        }
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            let masks = Masks::parse(&ui.get_mask_text());
            let mut found = Vec::new();
            collect_dir(&dir, ui.get_recurse(), &masks, &mut found);
            let mut s = st.borrow_mut();
            s.dirs = false;
            add_files(&mut s, found);
            drop(s);
            refresh(&ui, &st.borrow());
        }
    });

    on!(on_pick_dirs, |ui, st| {
        if mode_blocked(&ui, &st.borrow(), true) {
            return;
        }
        if let Some(paths) = rfd::FileDialog::new().pick_folders() {
            let mut s = st.borrow_mut();
            s.dirs = true;
            add_files(&mut s, paths);
            drop(s);
            refresh(&ui, &st.borrow());
        }
    });

    on!(on_clear_files, |ui, st| {
        let mut s = st.borrow_mut();
        s.files.clear();
        s.overrides.clear();
        s.dirs = false;
        drop(s);
        ui.set_selected_row(-1);
        ui.set_override_text("".into());
        refresh(&ui, &st.borrow());
    });

    on!(on_save_list, |ui, st| {
        let s = st.borrow();
        if s.files.is_empty() {
            ui.set_status_text("nothing to save".into());
            return;
        }
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("text", &["txt"])
            .set_file_name("filelist.txt")
            .save_file()
        {
            let body: String = s
                .files
                .iter()
                .map(|f| format!("{}\n", f.display()))
                .collect();
            let msg = match fs::write(&p, body) {
                Ok(_) => format!("saved {} item(s) to {}", s.files.len(), p.display()),
                Err(e) => format!("save failed: {e}"),
            };
            drop(s);
            ui.set_status_text(msg.into());
        }
    });

    on!(on_load_list, |ui, st| {
        let Some(p) = rfd::FileDialog::new()
            .add_filter("text", &["txt"])
            .pick_file()
        else {
            return;
        };
        let body = match fs::read_to_string(&p) {
            Ok(b) => b,
            Err(e) => {
                ui.set_status_text(format!("load failed: {e}").into());
                return;
            }
        };
        let paths: Vec<PathBuf> = body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        let total = paths.len();
        // A list is folders only if every line is one; otherwise keep the files.
        let dirs_mode = !paths.is_empty() && paths.iter().all(|p| p.is_dir());
        let keep: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| if dirs_mode { p.is_dir() } else { p.is_file() })
            .collect();
        let skipped = total - keep.len();
        let mut s = st.borrow_mut();
        s.files = keep;
        s.dirs = dirs_mode;
        s.overrides.clear();
        let n = s.files.len();
        drop(s);
        ui.set_selected_row(-1);
        refresh(&ui, &st.borrow());
        ui.set_status_text(
            match skipped {
                0 => format!("loaded {n} item(s)"),
                _ => format!("loaded {n} item(s), skipped {skipped} missing"),
            }
            .into(),
        );
    });

    // CSV import: column 1 = existing file path, column 2 = manual new name.
    on!(on_import_csv, |ui, st| {
        let Some(p) = rfd::FileDialog::new()
            .add_filter("csv", &["csv"])
            .pick_file()
        else {
            return;
        };
        let body = match fs::read_to_string(&p) {
            Ok(b) => b,
            Err(e) => {
                ui.set_status_text(format!("import failed: {e}").into());
                return;
            }
        };
        let mut files = Vec::new();
        let mut overrides = HashMap::new();
        let mut skipped = 0;
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let cols = crate::presets::csv_split(line);
            let path = PathBuf::from(cols[0].trim());
            if !path.is_file() {
                skipped += 1; // also skips a header row
                continue;
            }
            if let Some(new) = cols.get(1).map(|c| c.trim()).filter(|c| !c.is_empty()) {
                overrides.insert(path.clone(), new.to_string());
            }
            files.push(path);
        }
        let n = files.len();
        let mut s = st.borrow_mut();
        s.files = files;
        s.overrides = overrides;
        s.dirs = false;
        drop(s);
        ui.set_selected_row(-1);
        refresh(&ui, &st.borrow());
        ui.set_status_text(
            match skipped {
                0 => format!("imported {n} item(s) from CSV"),
                _ => format!("imported {n} item(s) from CSV, skipped {skipped} line(s)"),
            }
            .into(),
        );
    });

    on!(on_export_preview, |ui, st| {
        if st.borrow().files.is_empty() {
            ui.set_status_text("nothing to export".into());
            return;
        }
        let Some(p) = rfd::FileDialog::new()
            .add_filter("csv", &["csv"])
            .add_filter("json", &["json"])
            .add_filter("text", &["txt"])
            .set_file_name("preview.csv")
            .save_file()
        else {
            return;
        };
        let items = compute(&ui, &st.borrow()).items;
        let msg = match batch::export_preview(&items, &p) {
            Ok(_) => format!("preview exported to {}", p.display()),
            Err(e) => format!("export failed: {e}"),
        };
        ui.set_status_text(msg.into());
    });

    on!(on_save_preset, |ui, st| {
        if st.borrow().rules.is_empty() {
            ui.set_status_text("no rules to save".into());
            return;
        }
        let dir = crate::presets::dir();
        let _ = fs::create_dir_all(&dir);
        let Some(p) = rfd::FileDialog::new()
            .add_filter("preset", &["preset"])
            .set_directory(&dir)
            .set_file_name("rules.preset")
            .save_file()
        else {
            return;
        };
        let s = st.borrow();
        let preset = crate::presets::Preset {
            settings: [
                ("start".to_string(), ui.get_start_text().to_string()),
                ("pad".to_string(), ui.get_pad_text().to_string()),
                ("mode".to_string(), ui.get_batch_mode().to_string()),
                ("dest".to_string(), ui.get_dest_text().to_string()),
                ("collide".to_string(), ui.get_collide().to_string()),
                (
                    "collide_pattern".to_string(),
                    ui.get_collide_pattern().to_string(),
                ),
            ]
            .into(),
            rules: s
                .rules
                .iter()
                .map(|r| (r.kind.clone(), r.mods.clone(), r.a.clone(), r.b.clone()))
                .collect(),
        };
        let msg = match crate::presets::save(&p, &preset) {
            Ok(_) => format!("preset saved to {}", p.display()),
            Err(e) => format!("preset save failed: {e}"),
        };
        drop(s);
        ui.set_status_text(msg.into());
    });

    on!(on_load_preset, |ui, st| {
        let dir = crate::presets::dir();
        let _ = fs::create_dir_all(&dir);
        let Some(p) = rfd::FileDialog::new()
            .add_filter("preset", &["preset"])
            .set_directory(&dir)
            .pick_file()
        else {
            return;
        };
        apply_preset(&ui, st, &p);
    });

    on!(on_remove_selected, |ui, st| {
        let i = ui.get_selected_row();
        let mut s = st.borrow_mut();
        if i >= 0 && (i as usize) < s.files.len() {
            let p = s.files.remove(i as usize);
            s.overrides.remove(&p);
        }
        drop(s);
        ui.set_selected_row(-1);
        ui.set_override_text("".into());
        refresh(&ui, &st.borrow());
    });

    on!(on_move_selected, |ui, st, delta: i32| {
        let i = ui.get_selected_row();
        let j = i + delta;
        let moved = {
            let mut s = st.borrow_mut();
            if i >= 0 && j >= 0 && (i as usize) < s.files.len() && (j as usize) < s.files.len() {
                s.files.swap(i as usize, j as usize);
                true
            } else {
                false
            }
        };
        if moved {
            ui.set_selected_row(j);
            refresh(&ui, &st.borrow());
        }
    });

    on!(on_set_override, |ui, st| {
        let i = ui.get_selected_row();
        let text = ui.get_override_text().to_string();
        let mut s = st.borrow_mut();
        if i >= 0 && (i as usize) < s.files.len() {
            let p = s.files[i as usize].clone();
            if text.is_empty() {
                s.overrides.remove(&p);
            } else {
                s.overrides.insert(p, text);
            }
        }
        drop(s);
        refresh(&ui, &st.borrow());
    });

    on!(on_sort_changed, |ui, st| {
        let kind = ui.get_sort_by().to_string();
        let mut s = st.borrow_mut();
        match kind.as_str() {
            "name" => s.files.sort_by_cached_key(|f| natural_key(&name_of(f))),
            "ext" => s
                .files
                .sort_by_cached_key(|f| split_ext(&name_of(f)).1.to_lowercase()),
            "size" => s
                .files
                .sort_by_cached_key(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0)),
            "date" => s
                .files
                .sort_by_cached_key(|f| fs::metadata(f).and_then(|m| m.modified()).ok()),
            _ => return, // manual order
        }
        if ui.get_sort_desc() {
            s.files.reverse();
        }
        drop(s);
        ui.set_selected_row(-1);
        refresh(&ui, &st.borrow());
    });

    on!(on_search_changed, |ui, st| {
        refresh(&ui, &st.borrow());
    });

    on!(on_add_rule, |ui, st| {
        let kind = ui.get_new_kind().to_string();
        let (a, b) = match kind.as_str() {
            "case" => (ui.get_case_mode().to_string(), ui.get_field_b().to_string()),
            _ => (ui.get_field_a().to_string(), ui.get_field_b().to_string()),
        };
        // Option chips become the same mods the CLI takes as flag suffixes.
        let mut mods: Vec<String> = Vec::new();
        let part = ui.get_apply_part().to_string();
        if part != "both" {
            mods.push(part);
        }
        match kind.as_str() {
            "replace" => {
                if ui.get_replace_ci() {
                    mods.push("ci".into());
                }
                let occ = ui.get_replace_occ().to_string();
                if occ != "all" {
                    mods.push(occ);
                }
            }
            "trim" => {
                let at = ui.get_trim_at().to_string();
                if at != "both" {
                    mods.push(at);
                }
                if ui.get_trim_inv() {
                    mods.push("inv".into());
                }
            }
            _ => {}
        }
        let spec = RuleSpec {
            kind,
            a,
            b,
            mods: mods.join(":"),
        };
        match spec.build() {
            Ok(_) => {
                st.borrow_mut().rules.push(spec);
                ui.set_field_a("".into());
                ui.set_field_b("".into());
                refresh(&ui, &st.borrow());
            }
            Err(e) => ui.set_status_text(e.into()),
        }
    });

    on!(on_remove_rule, |ui, st, i: i32| {
        let mut s = st.borrow_mut();
        if (i as usize) < s.rules.len() {
            s.rules.remove(i as usize);
        }
        drop(s);
        refresh(&ui, &st.borrow());
    });

    on!(on_move_rule, |ui, st, from: i32, to: i32| {
        let mut s = st.borrow_mut();
        let (from, to) = (from as usize, to as isize);
        if from < s.rules.len() && to >= 0 && (to as usize) < s.rules.len() {
            s.rules.swap(from, to as usize);
        }
        drop(s);
        refresh(&ui, &st.borrow());
    });

    on!(on_numbering_changed, |ui, st| {
        refresh(&ui, &st.borrow());
    });

    on!(on_output_changed, |ui, st| {
        refresh(&ui, &st.borrow());
    });

    // Tag picker: append the tag to whichever field takes tag text.
    on!(on_insert_tag, |ui, st, tag: SharedString| {
        let _ = st;
        match ui.get_new_kind().as_str() {
            "replace" | "regex" => ui.set_field_b(format!("{}{tag}", ui.get_field_b()).into()),
            _ => ui.set_field_a(format!("{}{tag}", ui.get_field_a()).into()),
        }
    });

    // Selection: item details in the status bar, override field preloaded.
    on!(on_row_clicked, |ui, st, i: i32| {
        if i < 0 {
            ui.set_override_text("".into());
            return;
        }
        let s = st.borrow();
        let Some(f) = s.files.get(i as usize) else {
            return;
        };
        let msg = match fs::metadata(f) {
            Ok(md) => format!(
                "{} · {} bytes · created {} · modified {}",
                f.display(),
                md.len(),
                md.created().map(crate::tags::dt_string).unwrap_or_default(),
                md.modified()
                    .map(crate::tags::dt_string)
                    .unwrap_or_default()
            ),
            Err(_) => f.display().to_string(),
        };
        let over = s.overrides.get(f).cloned().unwrap_or_default();
        drop(s);
        ui.set_override_text(over.into());
        ui.set_status_text(msg.into());
    });

    on!(on_apply_batch, |ui, st| {
        // A bad timestamp spec blocks the batch before anything moves.
        let touch_spec = match ui.get_touch_text().trim() {
            "" => None,
            spec => match batch::parse_touch(spec) {
                Ok(t) => Some(t),
                Err(e) => {
                    ui.set_status_text(e.into());
                    return;
                }
            },
        };
        let c = compute(&ui, &st.borrow());
        let planned = c.plan.len();
        let res = batch::execute(c.plan, c.mode);
        let mut touched = String::new();
        if let Some(spec) = &touch_spec {
            let finals: Vec<PathBuf> = c
                .items
                .iter()
                .map(|it| {
                    res.renamed
                        .iter()
                        .find(|op| op.from == it.from)
                        .map(|op| op.to.clone())
                        .unwrap_or_else(|| it.from.clone())
                })
                .collect();
            let (n, errors) = batch::apply_touch(&finals, spec);
            touched = match errors.len() {
                0 => format!(" · timestamps set on {n}"),
                k => format!(" · timestamps set on {n}, {k} failed"),
            };
        }
        let done = match c.mode {
            Mode::Rename => "renamed",
            Mode::Copy => "copied",
            Mode::Move => "moved",
        };
        let mut warn = String::new();
        if c.mode != Mode::Copy {
            if let Err(e) = batch::record(&res.renamed) {
                warn = format!(" · history not written: {e}");
            }
            let mut s = st.borrow_mut();
            for op in &res.renamed {
                if let Some(f) = s.files.iter_mut().find(|f| **f == op.from) {
                    *f = op.to.clone();
                }
                s.overrides.remove(&op.from); // override served its purpose
            }
            if !res.renamed.is_empty() {
                s.can_undo = true;
            }
        }
        refresh(&ui, &st.borrow());
        ui.set_status_text(
            match res.failed.len() {
                0 => format!("{done} {} of {planned} item(s){touched}{warn}", res.renamed.len()),
                n => format!(
                    "{done} {} of {planned} item(s), {n} failed — kept in list for retry{touched}{warn}",
                    res.renamed.len()
                ),
            }
            .into(),
        );
    });

    on!(on_undo_batch, |ui, st| {
        // Reverts the most recent batch from persisted history, chains/swaps included.
        let msg = match batch::undo(None) {
            Ok((reverted, errors)) => {
                let mut s = st.borrow_mut();
                for op in &reverted {
                    if let Some(f) = s.files.iter_mut().find(|f| **f == op.from) {
                        *f = op.to.clone();
                    }
                }
                drop(s);
                match errors.len() {
                    0 => format!("reverted {} item(s)", reverted.len()),
                    n => format!(
                        "reverted {} item(s), {n} failed — undo again to retry",
                        reverted.len()
                    ),
                }
            }
            Err(e) => e,
        };
        st.borrow_mut().can_undo = !batch::history().is_empty();
        refresh(&ui, &st.borrow());
        ui.set_status_text(msg.into());
    });

    for p in initial {
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("preset")) {
            apply_preset(&ui, &state, &p);
        } else {
            handle_drop(&ui, &state, p);
        }
    }

    ui.run()
}

struct Computed {
    rows: Vec<FileRow>,
    items: Vec<batch::PlanItem>, // full preview, for export
    plan: Vec<Op>,               // only conflict-free changes
    mode: Mode,
    changed: i32,
    errors: i32,
}

fn compute(ui: &MainWindow, s: &State) -> Computed {
    let start: usize = ui.get_start_text().parse().unwrap_or(1);
    let pad: usize = ui
        .get_pad_text()
        .parse()
        .unwrap_or_else(|_| (start + s.files.len().max(1) - 1).to_string().len());
    let rules: Vec<RuleEntry> = s.rules.iter().filter_map(|r| r.build().ok()).collect();
    let mode = match ui.get_batch_mode().as_str() {
        "copy" => Mode::Copy,
        "move" => Mode::Move,
        _ => Mode::Rename,
    };
    let dest = if mode == Mode::Rename {
        String::new()
    } else {
        ui.get_dest_text().to_string()
    };
    let collision = match ui.get_collide().as_str() {
        "number" => Collision::Number,
        "letter" => Collision::Letter,
        "pattern" => {
            let p = ui.get_collide_pattern().to_string();
            Collision::Pattern(if p.is_empty() { "_<num>".into() } else { p })
        }
        _ => Collision::Fail,
    };
    let cfg = BatchCfg {
        rules: &rules,
        start,
        pad,
        overrides: &s.overrides,
        mode,
        dest: &dest,
        collision,
        pairs: ui.get_pairs(),
    };

    let items = batch::plan(&s.files, &cfg);
    let mut rows = Vec::new();
    let mut plan = Vec::new();
    let (mut changed, mut errors) = (0, 0);
    for (i, item) in items.iter().enumerate() {
        let (state, mut status) = match (item.changed, &item.issue) {
            (false, _) => (0, String::new()),
            (true, None) => (1, "ok".into()),
            (true, Some(e)) => (2, e.clone()),
        };
        if state == 1 && s.overrides.contains_key(&item.from) {
            status = "manual".into();
        }
        match state {
            1 => {
                changed += 1;
                plan.push(item.op());
            }
            2 => errors += 1,
            _ => {}
        }
        // Renames stay in place, so the name is enough; copy/move show the target path.
        let shown = if mode == Mode::Rename {
            item.new_name.clone()
        } else {
            item.target.display().to_string()
        };
        rows.push(FileRow {
            index: i as i32,
            old_name: name_of(&item.from).into(),
            new_name: if state == 0 {
                SharedString::new()
            } else {
                shown.into()
            },
            dir: item
                .from
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
                .into(),
            status: status.into(),
            state,
        });
    }
    Computed {
        rows,
        items,
        plan,
        mode,
        changed,
        errors,
    }
}

fn refresh(ui: &MainWindow, s: &State) {
    let c = compute(ui, s);
    ui.set_total(s.files.len() as i32);
    ui.set_changed(c.changed);
    ui.set_errors(c.errors);
    ui.set_can_undo(s.can_undo);
    // Search filters the view only; numbering and counts follow the full list.
    let q = ui.get_search_text().to_lowercase();
    let rows: Vec<FileRow> = if q.is_empty() {
        c.rows
    } else {
        c.rows
            .into_iter()
            .filter(|r| {
                r.old_name.to_lowercase().contains(&q) || r.new_name.to_lowercase().contains(&q)
            })
            .collect()
    };
    ui.set_files(ModelRc::new(VecModel::from(rows)));
    let rules: Vec<RuleRow> = s
        .rules
        .iter()
        .map(|r| RuleRow {
            kind: r.kind.to_uppercase().into(),
            summary: r.summary().into(),
        })
        .collect();
    ui.set_rules(ModelRc::new(VecModel::from(rules)));
    ui.set_status_text(
        format!(
            "{} file(s) · {} rule(s) · {} change(s)",
            s.files.len(),
            s.rules.len(),
            c.changed
        )
        .into(),
    );
}
