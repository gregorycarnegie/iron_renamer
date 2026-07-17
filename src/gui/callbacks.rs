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
use slint::{ComponentHandle, SharedString};
use std::{cell::RefCell, fs, path::Path, rc::Rc};

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
        let mut s = st.borrow_mut();
        if !sort_files(&mut s.files, &ui.get_sort_by()) {
            return;
        }
        if ui.get_sort_desc() {
            s.files.reverse();
        }
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
