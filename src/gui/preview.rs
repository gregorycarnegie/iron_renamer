use super::{FileRow, MainWindow, RuleRow, model::State};
use crate::{
    batch::{self, BatchCfg, Collision, Mode, Op},
    engine::{RuleEntry, name_of},
};
use slint::{ModelRc, SharedString, VecModel};

pub(super) struct Computed {
    rows: Vec<FileRow>,
    pub(super) items: Vec<batch::PlanItem>,
    pub(super) plan: Vec<Op>,
    pub(super) mode: Mode,
    changed: i32,
    errors: i32,
}

pub(super) fn compute(ui: &MainWindow, s: &State) -> Computed {
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
    let collision = Collision::parse(&ui.get_collide(), &ui.get_collide_pattern());
    let cfg = BatchCfg {
        rules: &rules,
        start,
        pad,
        overrides: &s.overrides,
        mode,
        dest: &dest,
        collision,
        pairs: ui.get_pairs(),
        csv: &[], // CSV import fills overrides, not <csv:COL> rows
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
            selected: s.sel.contains(&i),
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

pub(super) fn refresh(ui: &MainWindow, s: &State) {
    let c = compute(ui, s);
    ui.set_total(s.files.len() as i32);
    ui.set_changed(c.changed);
    ui.set_errors(c.errors);
    ui.set_can_undo(s.can_undo);
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
    ui.set_selection_count(s.sel.len() as i32);
    let rules: Vec<RuleRow> = s
        .rules
        .iter()
        .map(|r| RuleRow {
            kind: if r.kind == "pairs" {
                "LIST REPLACE".into()
            } else {
                r.kind.to_uppercase().into()
            },
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
