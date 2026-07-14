// Slint GUI front-end over the shared engine. Live preview on every change.

use crate::batch::{self, Op};
use crate::engine::{build_rule, name_of, natural_key, RuleEntry};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

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
        Ok(RuleEntry { rule, part, cond: None })
    }

    fn summary(&self) -> String {
        let b = |default: &str| if self.b.is_empty() { default.to_string() } else { self.b.clone() };
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
    dirs: bool, // list holds folders, not files — never mixed
    can_undo: bool,
}

pub fn run() -> Result<(), slint::PlatformError> {
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

    // One batch never mixes files and folders.
    fn mode_blocked(ui: &MainWindow, s: &State, want_dirs: bool) -> bool {
        if !s.files.is_empty() && s.dirs != want_dirs {
            ui.set_status_text(
                if s.dirs { "list holds folders — Clear it before adding files" } else { "list holds files — Clear it before adding folders" }.into(),
            );
            return true;
        }
        false
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
            let files = fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            let mut s = st.borrow_mut();
            s.dirs = false;
            add_files(&mut s, files);
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
        s.dirs = false;
        drop(s);
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
        let spec = RuleSpec { kind, a, b, mods: mods.join(":") };
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

    on!(on_apply_batch, |ui, st| {
        let plan = compute(&ui, &st.borrow()).plan;
        let planned = plan.len();
        let res = batch::execute(plan);
        let mut warn = String::new();
        if let Err(e) = batch::record(&res.renamed) {
            warn = format!(" · history not written: {e}");
        }
        let mut s = st.borrow_mut();
        for op in &res.renamed {
            if let Some(f) = s.files.iter_mut().find(|f| **f == op.from) {
                *f = op.to.clone();
            }
        }
        if !res.renamed.is_empty() {
            s.can_undo = true;
        }
        drop(s);
        refresh(&ui, &st.borrow());
        ui.set_status_text(
            match res.failed.len() {
                0 => format!("renamed {} of {planned} item(s){warn}", res.renamed.len()),
                n => format!(
                    "renamed {} of {planned} item(s), {n} failed — kept in list for retry{warn}",
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
                    n => format!("reverted {} item(s), {n} failed — undo again to retry", reverted.len()),
                }
            }
            Err(e) => e,
        };
        st.borrow_mut().can_undo = !batch::history().is_empty();
        refresh(&ui, &st.borrow());
        ui.set_status_text(msg.into());
    });

    ui.run()
}

fn add_files(s: &mut State, paths: Vec<PathBuf>) {
    for p in paths {
        if !s.files.contains(&p) {
            s.files.push(p);
        }
    }
    s.files.sort_by_key(|f| natural_key(&name_of(f)));
}

struct Computed {
    rows: Vec<FileRow>,
    plan: Vec<Op>, // only conflict-free changes
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

    let items = batch::plan(&s.files, &rules, start, pad);
    let mut rows = Vec::new();
    let mut plan = Vec::new();
    let (mut changed, mut errors) = (0, 0);
    for (i, item) in items.iter().enumerate() {
        let (state, status) = match (item.changed, &item.issue) {
            (false, _) => (0, String::new()),
            (true, None) => (1, "ok".into()),
            (true, Some(e)) => (2, e.clone()),
        };
        match state {
            1 => {
                changed += 1;
                plan.push(item.op());
            }
            2 => errors += 1,
            _ => {}
        }
        rows.push(FileRow {
            index: i as i32,
            old_name: name_of(&item.from).into(),
            new_name: if state == 0 { SharedString::new() } else { item.new_name.as_str().into() },
            dir: item.from.parent().map(|p| p.display().to_string()).unwrap_or_default().into(),
            status: status.into(),
            state,
        });
    }
    Computed { rows, plan, changed, errors }
}

fn refresh(ui: &MainWindow, s: &State) {
    let c = compute(ui, s);
    ui.set_total(s.files.len() as i32);
    ui.set_changed(c.changed);
    ui.set_errors(c.errors);
    ui.set_can_undo(s.can_undo);
    ui.set_files(ModelRc::new(VecModel::from(c.rows)));
    let rules: Vec<RuleRow> = s
        .rules
        .iter()
        .map(|r| RuleRow { kind: r.kind.to_uppercase().into(), summary: r.summary().into() })
        .collect();
    ui.set_rules(ModelRc::new(VecModel::from(rules)));
    ui.set_status_text(
        format!("{} file(s) · {} rule(s) · {} change(s)", s.files.len(), s.rules.len(), c.changed).into(),
    );
}
