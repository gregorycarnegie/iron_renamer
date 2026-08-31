use super::{
    MainWindow,
    model::{
        RuleSpec, State, add_files, move_rule, parse_csv_import, parse_list, remove_rule, retarget,
        specs_from_preset,
    },
    preview::{compute, refresh},
};
use crate::{
    batch::{self, Mode},
    engine::{Masks, collect_dir, sort_files},
};
use slint::{ComponentHandle, Model, SharedString};
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

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

// Flip only the `selected` flags on the existing rows — no preview recompute,
// so click-selection stays instant on big lists.
fn sync_selection(ui: &MainWindow, s: &State) {
    let files = ui.get_files();
    for i in 0..files.row_count() {
        if let Some(mut row) = files.row_data(i) {
            let want = s.sel.contains(&(row.index as usize));
            if row.selected != want {
                row.selected = want;
                files.set_row_data(i, row);
            }
        }
    }
    ui.set_selection_count(s.sel.len() as i32);
}

pub(super) fn apply_preset(ui: &MainWindow, st: &Rc<RefCell<State>>, p: &Path) {
    match crate::presets::load(p) {
        Ok(preset) => {
            let (specs, bad) = specs_from_preset(preset.rules);
            let n = specs.len();
            {
                let mut s = st.borrow_mut();
                s.rules = specs;
                s.editing = None;
            }
            ui.set_editing_rule(-1);
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

pub(super) fn wire(ui: &MainWindow, state: &Rc<RefCell<State>>) {
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
        s.sel.clear();
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
        let (keep, dirs_mode, skipped) = parse_list(&body);
        let mut s = st.borrow_mut();
        s.files = keep;
        s.dirs = dirs_mode;
        s.overrides.clear();
        s.sel.clear();
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
        let (files, overrides, skipped) = parse_csv_import(&body);
        let n = files.len();
        let mut s = st.borrow_mut();
        s.files = files;
        s.overrides = overrides;
        s.sel.clear();
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
        let mut s = st.borrow_mut();
        let idx: Vec<usize> = if s.sel.is_empty() {
            let i = ui.get_selected_row();
            if i >= 0 && (i as usize) < s.files.len() {
                vec![i as usize]
            } else {
                Vec::new()
            }
        } else {
            // descending, so earlier removals don't shift later indices
            s.sel
                .iter()
                .rev()
                .copied()
                .filter(|&k| k < s.files.len())
                .collect()
        };
        for k in idx {
            let p = s.files.remove(k);
            s.overrides.remove(&p);
        }
        s.sel.clear();
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
            let mut s = st.borrow_mut();
            let (a, b) = (i as usize, j as usize);
            let had_a = s.sel.remove(&a);
            let had_b = s.sel.remove(&b);
            if had_a {
                s.sel.insert(b);
            }
            if had_b {
                s.sel.insert(a);
            }
            if s.anchor == a {
                s.anchor = b;
            } else if s.anchor == b {
                s.anchor = a;
            }
            drop(s);
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
        let mut s = st.borrow_mut();
        if !sort_files(&mut s.files, &ui.get_sort_by()) {
            return;
        }
        if ui.get_sort_desc() {
            s.files.reverse();
        }
        s.sel.clear();
        drop(s);
        ui.set_selected_row(-1);
        refresh(&ui, &st.borrow());
    });

    on!(on_add_rule, |ui, st| {
        let kind = ui.get_new_kind().to_string();
        let (a, b) = match kind.as_str() {
            "case" => (ui.get_case_mode().to_string(), ui.get_field_b().to_string()),
            _ => (ui.get_field_a().to_string(), ui.get_field_b().to_string()),
        };
        let mut mods: Vec<String> = Vec::new();
        let part = ui.get_apply_part().to_string();
        if part != "both" {
            mods.push(part);
        }
        match kind.as_str() {
            "replace" => {
                if !ui.get_replace_cs() {
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
            "pairs" if !ui.get_pairs_cs() => mods.push("ci".into()),
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
                let mut s = st.borrow_mut();
                match s.editing.take() {
                    Some(i) if i < s.rules.len() => s.rules[i] = spec,
                    _ => s.rules.push(spec),
                }
                drop(s);
                ui.set_editing_rule(-1);
                ui.set_field_a("".into());
                ui.set_field_b("".into());
                refresh(&ui, &st.borrow());
            }
            Err(e) => ui.set_status_text(e.into()),
        }
    });

    on!(on_edit_rule, |ui, st, i: i32| {
        let i = i as usize;
        let spec = {
            let mut s = st.borrow_mut();
            if s.editing == Some(i) {
                s.editing = None;
                ui.set_editing_rule(-1);
                return;
            }
            let Some(spec) = s.rules.get(i).cloned() else {
                return;
            };
            s.editing = Some(i);
            spec
        };
        ui.set_apply_part("both".into());
        ui.set_replace_cs(true);
        ui.set_replace_occ("all".into());
        ui.set_pairs_cs(true);
        ui.set_trim_at("both".into());
        ui.set_trim_inv(false);
        for m in spec.mods.split(':').filter(|m| !m.is_empty()) {
            match m {
                "name" | "stem" => ui.set_apply_part("name".into()),
                "ext" => ui.set_apply_part("ext".into()),
                "ci" if spec.kind == "pairs" => ui.set_pairs_cs(false),
                "ci" => ui.set_replace_cs(false),
                "first" | "last" => ui.set_replace_occ(m.into()),
                "start" | "end" | "all" => ui.set_trim_at(m.into()),
                "inv" => ui.set_trim_inv(true),
                _ => {}
            }
        }
        if spec.kind == "case" {
            ui.set_case_mode(spec.a.into());
            ui.set_field_a("".into());
        } else {
            ui.set_field_a(spec.a.into());
        }
        ui.set_field_b(spec.b.into());
        ui.set_new_kind(spec.kind.into());
        ui.set_editing_rule(i as i32);
    });

    on!(on_cancel_edit, |ui, st| {
        st.borrow_mut().editing = None;
        ui.set_editing_rule(-1);
        ui.set_field_a("".into());
        ui.set_field_b("".into());
    });

    on!(on_remove_rule, |ui, st, i: i32| {
        let mut s = st.borrow_mut();
        remove_rule(&mut s, i as usize);
        ui.set_editing_rule(s.editing.map_or(-1, |e| e as i32));
        drop(s);
        refresh(&ui, &st.borrow());
    });

    on!(on_clear_rules, |ui, st| {
        let mut s = st.borrow_mut();
        s.rules.clear();
        s.editing = None;
        drop(s);
        ui.set_editing_rule(-1);
        refresh(&ui, &st.borrow());
    });

    on!(on_move_rule, |ui, st, from: i32, to: i32| {
        let mut s = st.borrow_mut();
        move_rule(&mut s, from as usize, to as isize);
        ui.set_editing_rule(s.editing.map_or(-1, |e| e as i32));
        drop(s);
        refresh(&ui, &st.borrow());
    });

    on!(on_settings_changed, |ui, st| {
        refresh(&ui, &st.borrow());
    });

    on!(on_insert_tag, |ui, st, tag: SharedString| {
        let _ = st;
        match ui.get_new_kind().as_str() {
            "replace" | "regex" => ui.set_field_b(format!("{}{tag}", ui.get_field_b()).into()),
            _ => ui.set_field_a(format!("{}{tag}", ui.get_field_a()).into()),
        }
    });

    on!(on_select_row, |ui, st, i: i32, ctrl: bool, shift: bool| {
        let mut s = st.borrow_mut();
        if i < 0 || (i as usize) >= s.files.len() {
            s.sel.clear();
            drop(s);
            ui.set_selected_row(-1);
            ui.set_override_text("".into());
            sync_selection(&ui, &st.borrow());
            return;
        }
        let idx = i as usize;
        if shift {
            let a = s.anchor.min(s.files.len() - 1);
            if !ctrl {
                s.sel.clear();
            }
            for k in a.min(idx)..=a.max(idx) {
                s.sel.insert(k);
            }
        } else if ctrl {
            if !s.sel.remove(&idx) {
                s.sel.insert(idx);
            }
            s.anchor = idx;
        } else {
            s.sel.clear();
            s.sel.insert(idx);
            s.anchor = idx;
        }
        drop(s);
        ui.set_selected_row(i);
        sync_selection(&ui, &st.borrow());
        let s = st.borrow();
        if s.sel.len() > 1 {
            ui.set_override_text("".into());
            ui.set_status_text(format!("{} of {} selected", s.sel.len(), s.files.len()).into());
            return;
        }
        let Some(f) = s.files.get(idx) else { return };
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

    on!(on_row_right, |ui, st, i: i32| {
        let mut s = st.borrow_mut();
        if i < 0 || (i as usize) >= s.files.len() {
            return;
        }
        if !s.sel.contains(&(i as usize)) {
            s.sel.clear();
            s.sel.insert(i as usize);
            s.anchor = i as usize;
        }
        let over = s
            .files
            .get(i as usize)
            .and_then(|f| s.overrides.get(f))
            .cloned()
            .unwrap_or_default();
        let single = s.sel.len() == 1;
        drop(s);
        ui.set_selected_row(i);
        if single {
            ui.set_override_text(over.into());
        }
        sync_selection(&ui, &st.borrow());
    });

    on!(on_reveal_file, |ui, st, i: i32| {
        let _ = &ui;
        let s = st.borrow();
        let Some(p) = s.files.get(i as usize) else {
            return;
        };
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("explorer")
                .raw_arg(format!("/select,\"{}\"", p.display()))
                .spawn();
        }
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open")
            .arg(p.parent().unwrap_or(Path::new(".")))
            .spawn();
    });

    on!(on_keep_selected, |ui, st| {
        let mut s = st.borrow_mut();
        if s.sel.is_empty() {
            return;
        }
        let keep: Vec<PathBuf> = s
            .sel
            .iter()
            .filter_map(|&k| s.files.get(k).cloned())
            .collect();
        s.overrides.retain(|p, _| keep.contains(p));
        s.files = keep;
        s.sel.clear();
        drop(s);
        ui.set_selected_row(-1);
        ui.set_override_text("".into());
        refresh(&ui, &st.borrow());
    });

    on!(on_apply_batch, |ui, st| {
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
            let (n, errors) = batch::apply_touch(&batch::finals(&c.items, &res), spec);
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
            retarget(&mut s, &res.renamed);
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
        let msg = match batch::undo(None) {
            Ok((reverted, errors)) => {
                retarget(&mut st.borrow_mut(), &reverted);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // mode_blocked guards the file-picking callbacks, which open a native
    // dialog once they get past it, so it is exercised directly here rather
    // than through invoke_pick_files.
    fn ui() -> MainWindow {
        i_slint_backend_testing::init_no_event_loop();
        MainWindow::new().unwrap()
    }

    #[test]
    fn mode_is_only_blocked_when_a_non_empty_list_disagrees() {
        let ui = ui();
        let mut s = State::default();

        // An empty list accepts either kind.
        assert!(!mode_blocked(&ui, &s, false));
        assert!(!mode_blocked(&ui, &s, true));

        // A list of files blocks adding folders, and says so.
        s.files.push(PathBuf::from("/t/a.txt"));
        s.dirs = false;
        assert!(!mode_blocked(&ui, &s, false), "files accept more files");
        assert!(mode_blocked(&ui, &s, true), "files must block folders");
        assert!(
            ui.get_status_text()
                .contains("Clear it before adding folders")
        );

        // ...and the mirror case.
        s.dirs = true;
        assert!(!mode_blocked(&ui, &s, true), "folders accept more folders");
        assert!(mode_blocked(&ui, &s, false), "folders must block files");
        assert!(
            ui.get_status_text()
                .contains("Clear it before adding files")
        );
    }

    #[test]
    fn selection_flags_follow_the_state_without_recomputing_rows() {
        let ui = ui();
        let state = Rc::new(RefCell::new(State::default()));
        state.borrow_mut().files = (0..4)
            .map(|i| PathBuf::from(format!("/t/f{i}.txt")))
            .collect();
        refresh(&ui, &state.borrow());
        assert_eq!(ui.get_selection_count(), 0);

        {
            let mut s = state.borrow_mut();
            s.sel.insert(1);
            s.sel.insert(3);
        }
        sync_selection(&ui, &state.borrow());
        let flags: Vec<bool> = ui.get_files().iter().map(|r| r.selected).collect();
        assert_eq!(flags, [false, true, false, true]);
        assert_eq!(ui.get_selection_count(), 2);

        // Clearing flips them back rather than leaving them stuck on.
        state.borrow_mut().sel.clear();
        sync_selection(&ui, &state.borrow());
        let flags: Vec<bool> = ui.get_files().iter().map(|r| r.selected).collect();
        assert_eq!(flags, [false; 4]);
        assert_eq!(ui.get_selection_count(), 0);
    }

    #[test]
    fn applying_a_preset_loads_rules_settings_and_clears_the_edit_cursor() {
        let ui = ui();
        let state = Rc::new(RefCell::new(State::default()));
        state.borrow_mut().files.push(PathBuf::from("/t/a.txt"));
        // Something is mid-edit when the preset arrives.
        state.borrow_mut().editing = Some(0);
        ui.set_editing_rule(0);

        let dir = std::env::temp_dir().join(format!("iron_cb_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("good.preset");
        std::fs::write(
            &p,
            "rule\tpattern\t\tpic_<num>.<ext>\t\nset\tstart\t5\nset\tcollide\tnumber\n",
        )
        .unwrap();
        apply_preset(&ui, &state, &p);

        assert_eq!(state.borrow().rules.len(), 1);
        assert_eq!(ui.get_start_text(), "5");
        assert_eq!(ui.get_collide(), "number");
        // Defaults fill in for settings the preset does not carry.
        assert_eq!(ui.get_batch_mode(), "rename");
        // The edit cursor is cancelled, not moved to row 1.
        assert_eq!(ui.get_editing_rule(), -1);
        assert_eq!(state.borrow().editing, None);
        // All rules were valid, so the status must not claim otherwise.
        assert!(ui.get_status_text().contains("loaded 1 rule(s)"));
        assert!(
            !ui.get_status_text().contains("invalid"),
            "{}",
            ui.get_status_text()
        );

        // A preset with a broken rule reports the count that failed.
        let bad = dir.join("bad.preset");
        std::fs::write(&bad, "rule\tregex\t[unclosed\tx\t\n").unwrap();
        apply_preset(&ui, &state, &bad);
        assert!(
            ui.get_status_text().contains("invalid"),
            "{}",
            ui.get_status_text()
        );

        // A file that is not a preset at all reports the load error.
        let missing = dir.join("nope.preset");
        let _ = std::fs::remove_file(&missing);
        apply_preset(&ui, &state, &missing);
        assert!(
            !ui.get_status_text().contains("loaded"),
            "{}",
            ui.get_status_text()
        );
    }
}
