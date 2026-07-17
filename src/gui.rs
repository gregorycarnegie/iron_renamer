// Slint GUI front-end over the shared engine. Live preview on every change.

use crate::{
    batch::{self, BatchCfg, Collision, Mode, Op},
    engine::{
        FsKinds, Masks, RuleEntry, build_rule, collect_dir, name_of, natural_key, sort_files,
    },
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
};

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
struct State {
    files: Vec<PathBuf>,
    rules: Vec<RuleSpec>,
    overrides: HashMap<PathBuf, String>, // per-item manual new names
    dirs: bool,                          // list holds folders, not files — never mixed
    can_undo: bool,
    editing: Option<usize>, // stack index loaded into the rule form, if any
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

// Point list entries at their post-batch locations; consumed (or stale,
// after undo) overrides go with them.
fn retarget(s: &mut State, ops: &[Op]) {
    for op in ops {
        if let Some(f) = s.files.iter_mut().find(|f| **f == op.from) {
            *f = op.to.clone();
        }
        s.overrides.remove(&op.from);
    }
}

// Drop a rule from the stack; the edit highlight follows or clears.
fn remove_rule(s: &mut State, i: usize) {
    if i < s.rules.len() {
        s.rules.remove(i);
        s.editing = match s.editing {
            Some(e) if e == i => None,
            Some(e) if e > i => Some(e - 1),
            other => other,
        };
    }
}

// Swap two rules; the edit highlight follows the rule it belongs to.
fn move_rule(s: &mut State, from: usize, to: isize) {
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
fn parse_list(body: &str) -> (Vec<PathBuf>, bool, usize) {
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

// CSV import: column 1 = existing file path, column 2 = optional manual
// new name. Missing files (and thus any header row) are skipped and counted.
fn parse_csv_import(body: &str) -> (Vec<PathBuf>, HashMap<PathBuf, String>, usize) {
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

// Preset rules become specs; unparsable ones are counted, not loaded.
fn specs_from_preset(rules: Vec<(String, String, String, String)>) -> (Vec<RuleSpec>, usize) {
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

// What a background drop scan found, ready to apply on the UI thread.
struct DropScan {
    add: Vec<PathBuf>,
    folder_mode: bool, // list was in folder mode: folders added, files blocked
    blocked: usize,    // paths skipped by the file/folder mode wall
}

// OS drag-and-drop: files add as files; a folder adds its contents with the
// mask/recurse settings as of drop time, unless the list is already in folder
// mode. All path I/O runs on a worker thread — on a network share, classifying
// hundreds of paths is seconds of round trips and would freeze the UI — and
// the result lands back on the UI thread via a short poll timer.
fn handle_drop(ui: &MainWindow, st: &Rc<RefCell<State>>, paths: Vec<PathBuf>) {
    let folder_mode = {
        let s = st.borrow();
        s.dirs && !s.files.is_empty()
    };
    let masks = Masks::parse(&ui.get_mask_text());
    let recurse = ui.get_recurse();
    ui.set_status_text(format!("scanning {} dropped item(s)…", paths.len()).into());

    let slot: Arc<Mutex<Option<DropScan>>> = Arc::new(Mutex::new(None));
    {
        let slot = slot.clone();
        std::thread::spawn(move || {
            let mut kinds = FsKinds::new();
            kinds.warm_parents(&paths);
            let mut scan = DropScan {
                add: Vec::new(),
                folder_mode,
                blocked: 0,
            };
            for path in paths {
                match (kinds.kind(&path) == Some(true), folder_mode) {
                    (true, true) => scan.add.push(path),
                    (true, false) => collect_dir(&path, recurse, &masks, &mut scan.add),
                    (false, true) => scan.blocked += 1,
                    (false, false) => scan.add.push(path),
                }
            }
            *slot.lock().unwrap() = Some(scan);
        });
    }

    // ponytail: the timer keeps itself alive through its own Rc and goes inert
    // after one apply; the dead allocation per drop is negligible.
    let weak = ui.as_weak();
    let st = st.clone();
    let timer = Rc::new(slint::Timer::default());
    let alive = timer.clone();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            let Some(scan) = slot.lock().unwrap().take() else {
                return;
            };
            alive.stop();
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                if !scan.folder_mode {
                    s.dirs = false;
                }
                add_files(&mut s, scan.add);
            }
            refresh(&ui, &st.borrow());
            if scan.blocked > 0 {
                ui.set_status_text("list holds folders — Clear it before adding files".into());
            }
        },
    );
}

// Load a preset file into the rule list and settings (Load-preset button,
// initial .preset argument, or .preset file association).
fn apply_preset(ui: &MainWindow, st: &Rc<RefCell<State>>, p: &Path) {
    match crate::presets::load(p) {
        Ok(preset) => {
            let (specs, bad) = specs_from_preset(preset.rules);
            let n = specs.len();
            {
                let mut s = st.borrow_mut();
                s.rules = specs;
                s.editing = None; // the stack was replaced wholesale
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

/// `initial` paths (from the Explorer context menu or `iron_renamer gui`)
/// load as if dropped on the window; a .preset file loads as a preset.
pub fn run(initial: Vec<PathBuf>) -> Result<(), slint::PlatformError> {
    // Frameless window with a custom in-app title bar, VS Code style; the
    // decorations themselves come off via the Window's `no-frame` property.
    // macOS keeps the native frame: winit can't drag-resize there and the
    // traffic lights belong on the left. Hooked before window creation.
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attrs| {
            let attrs = attrs.with_theme(Some(slint::winit_030::winit::window::Theme::Dark));
            #[cfg(target_os = "windows")]
            let attrs = {
                use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
                attrs.with_undecorated_shadow(true)
            };
            attrs
        })
        .select()?;
    let ui = MainWindow::new()?;
    ui.set_frameless(!cfg!(target_os = "macos"));
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.on_open_url(|url| {
        let url = url.to_string();
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .spawn();
        }
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&url).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    });
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
        // One DroppedFile event arrives per file; refreshing on each makes a big
        // drop O(N²). Buffer the burst and debounce: each file restarts the
        // timer, so one refresh runs shortly after the last file lands. (A
        // zero-delay single-shot is not enough — winit can run timers between
        // individual drop events.)
        let pending: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(Vec::new()));
        let debounce = Rc::new(slint::Timer::default());
        ui.window().on_winit_window_event(move |_, ev| {
            if let Some(ui) = weak.upgrade() {
                match ev {
                    WindowEvent::DroppedFile(path) => {
                        pending.borrow_mut().push(path.clone());
                        let weak = weak.clone();
                        let st = st.clone();
                        let pending = pending.clone();
                        debounce.start(
                            slint::TimerMode::SingleShot,
                            std::time::Duration::from_millis(100),
                            move || {
                                if let Some(ui) = weak.upgrade() {
                                    let paths = std::mem::take(&mut *pending.borrow_mut());
                                    handle_drop(&ui, &st, paths);
                                }
                            },
                        );
                    }
                    // The custom title bar's move/resize (`drag_window` /
                    // `drag_resize_window` above) and focus changes hand the pointer
                    // to the window manager for a while; on Linux winit doesn't
                    // always see the matching button-release when it gets control
                    // back, so Slint's grab/hover state can get stuck until some
                    // unrelated click happens to resync it. Force the reset directly
                    // whenever focus changes or a resize/move settles.
                    WindowEvent::Focused(_) | WindowEvent::Resized(_) | WindowEvent::Moved(_) => {
                        ui.window()
                            .dispatch_event(slint::platform::WindowEvent::PointerExited);
                    }
                    _ => {}
                }
            }
            EventResult::Propagate
        });
    }

    // Custom title bar: close, and the native move/resize drag loops.
    {
        use slint::winit_030::{WinitWindowAccessor, winit::window::ResizeDirection};

        let weak = ui.as_weak();
        ui.on_win_close(move || weak.unwrap().window().hide().unwrap());

        // A drag starts on press so the native move loop takes over, which
        // swallows TouchArea's double-clicked — so detect the double click
        // ourselves: two presses within 400ms toggle maximize instead.
        let weak = ui.as_weak();
        let last_press = std::cell::Cell::new(None::<std::time::Instant>);
        ui.on_win_drag(move || {
            let ui = weak.unwrap();
            let w = ui.window();
            if last_press
                .get()
                .is_some_and(|t| t.elapsed().as_millis() < 400)
            {
                last_press.set(None);
                w.set_maximized(!w.is_maximized());
            } else if !w.is_maximized() {
                // ponytail: dragging a maximized window doesn't restore-then-drag like
                // native title bars; add restore-on-drag if anyone misses it
                last_press.set(Some(std::time::Instant::now()));
                w.with_winit_window(|w| {
                    let _ = w.drag_window();
                });
            }
        });

        let weak = ui.as_weak();
        ui.on_win_resize(move |dir| {
            let dir = match dir.as_str() {
                "n" => ResizeDirection::North,
                "s" => ResizeDirection::South,
                "e" => ResizeDirection::East,
                "w" => ResizeDirection::West,
                "ne" => ResizeDirection::NorthEast,
                "nw" => ResizeDirection::NorthWest,
                "sw" => ResizeDirection::SouthWest,
                _ => ResizeDirection::SouthEast,
            };
            weak.unwrap().window().with_winit_window(|w| {
                let _ = w.drag_resize_window(dir);
            });
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
            return; // manual order
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
        // Option chips become the same mods the CLI takes as flag suffixes.
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

    // Click a stack row: load it into the form; Save replaces it in place.
    // Clicking the row being edited cancels.
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
        // Option chips: defaults first, then the rule's mods.
        ui.set_apply_part("both".into());
        // No "ci" mod on the rule means the engine runs it case-sensitively.
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
                _ => {} // n<N>/pad<N> have no chip; saving drops them
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

    // Any numbering/output/search edit just recomputes the preview.
    on!(on_settings_changed, |ui, st| {
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
        // Reverts the most recent batch from persisted history, chains/swaps included.
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

    let (presets, files): (Vec<_>, Vec<_>) = initial.into_iter().partition(|p| {
        p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("preset"))
    });
    for p in presets {
        apply_preset(&ui, &state, &p);
    }
    if !files.is_empty() {
        handle_drop(&ui, &state, files);
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
        csv: &[], // the GUI's CSV import fills overrides, not <csv:COL> rows
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
            // engine kind "pairs" wears its AR-familiar name in the UI
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

#[cfg(test)]
mod tests {
    use super::*;

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

        // Every line a folder -> folder mode.
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
            ("regex".into(), String::new(), "[".into(), String::new()), // bad regex
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

        remove_rule(&mut state, 0); // rule before the edited one -> index shifts
        assert_eq!((state.rules.len(), state.editing), (2, Some(0)));

        move_rule(&mut state, 0, 1); // edited rule moves -> highlight follows
        assert_eq!(state.editing, Some(1));
        assert_eq!(state.rules[1].kind, "trim");

        move_rule(&mut state, 0, 5); // out of range -> no-op
        assert_eq!(state.editing, Some(1));

        remove_rule(&mut state, 1); // the edited rule itself -> highlight clears
        assert_eq!((state.rules.len(), state.editing), (1, None));
    }
}
