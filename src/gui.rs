// Slint GUI front-end over the shared engine. Live preview on every change.

use crate::{
    batch,
    engine::{FsKinds, Masks, collect_dir},
};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

slint::include_modules!();

mod callbacks;
mod model;
mod preview;

use callbacks::apply_preset;
use model::{State, add_files};
use preview::refresh;

struct DropScan {
    add: Vec<PathBuf>,
    folder_mode: bool,
    blocked: usize,
}

// All path I/O runs off the UI thread; network-share lookups can take seconds.
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

/// `initial` paths (from Explorer or `iron_renamer gui`) load as if dropped;
/// a .preset file loads as a preset.
pub fn run(initial: Vec<PathBuf>) -> Result<(), slint::PlatformError> {
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
                .creation_flags(0x0800_0000)
                .spawn();
        }
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&url).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    });
    let state = Rc::new(RefCell::new(State::default()));
    state.borrow_mut().can_undo = !batch::history().is_empty();

    // OS file drop (winit emits one event per file, so debounce the burst).
    {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        let weak = ui.as_weak();
        let st = state.clone();
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

    // Custom title bar: close and native move/resize drag loops.
    {
        use slint::winit_030::{WinitWindowAccessor, winit::window::ResizeDirection};

        let weak = ui.as_weak();
        ui.on_win_close(move || weak.unwrap().window().hide().unwrap());

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
                // ponytail: add restore-on-drag if anyone misses native behavior
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

    callbacks::wire(&ui, &state);

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

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    // The winit bits in `run` (backend selector, window drag/resize) can't work
    // headless, but everything below `callbacks::wire` can: the testing backend
    // gives MainWindow::new() a platform, and the generated setters/invokers
    // drive the app the way the .slint file does.
    #[test]
    fn add_rule_callback_renames_the_loaded_files() {
        i_slint_backend_testing::init_no_event_loop();

        let ui = MainWindow::new().unwrap();
        let state = Rc::new(RefCell::new(model::State::default()));
        add_files(
            &mut state.borrow_mut(),
            vec![PathBuf::from("/t/a b.txt"), PathBuf::from("/t/c d.txt")],
        );
        callbacks::wire(&ui, &state);
        preview::refresh(&ui, &state.borrow());
        assert_eq!(ui.get_total(), 2);
        assert_eq!(ui.get_changed(), 0);

        ui.set_new_kind("replace".into());
        ui.set_field_a(" ".into());
        ui.set_field_b("_".into());
        ui.invoke_add_rule();

        let rows: Vec<_> = ui.get_files().iter().collect();
        assert_eq!(ui.get_changed(), 2);
        assert_eq!(rows[0].new_name, "a_b.txt");
        assert_eq!(rows[1].new_name, "c_d.txt");
        // The form clears itself after a rule is accepted.
        assert_eq!(ui.get_field_a(), "");
    }
    // Second test proving the backend is per-thread: each #[test] gets a fresh
    // thread, so every one of them may call init_no_event_loop().
    #[test]
    fn clear_rules_callback_restores_the_original_names() {
        i_slint_backend_testing::init_no_event_loop();

        let ui = MainWindow::new().unwrap();
        let state = Rc::new(RefCell::new(model::State::default()));
        add_files(&mut state.borrow_mut(), vec![PathBuf::from("/t/a b.txt")]);
        callbacks::wire(&ui, &state);

        ui.set_new_kind("case".into());
        ui.set_case_mode("upper".into());
        ui.invoke_add_rule();
        assert_eq!(ui.get_files().iter().next().unwrap().new_name, "A B.TXT");

        ui.invoke_clear_rules();
        assert_eq!(ui.get_changed(), 0);
        // An unchanged row blanks its new_name; the UI leaves that column empty.
        assert_eq!(ui.get_files().iter().next().unwrap().new_name, "");
    }

    fn seeded(paths: &[&str]) -> (MainWindow, Rc<RefCell<model::State>>) {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        let state = Rc::new(RefCell::new(model::State::default()));
        add_files(
            &mut state.borrow_mut(),
            paths.iter().map(PathBuf::from).collect(),
        );
        callbacks::wire(&ui, &state);
        preview::refresh(&ui, &state.borrow());
        (ui, state)
    }

    #[test]
    fn automatic_pad_width_spans_start_through_last_index() {
        let nine: Vec<String> = (1..=9).map(|i| format!("/t/f{i}.txt")).collect();
        let (ui, state) = seeded(&nine.iter().map(String::as_str).collect::<Vec<_>>());
        ui.set_new_kind("pattern".into());
        ui.set_field_a("<num>.<ext>".into());
        ui.invoke_add_rule();
        // start 1 over 9 files ends at 9: one digit, no padding.
        assert_eq!(ui.get_files().iter().next().unwrap().new_name, "1.txt");

        // Starting at 2 pushes the last index to 10, so every name widens.
        ui.set_start_text("2".into());
        ui.invoke_settings_changed();
        preview::refresh(&ui, &state.borrow());
        assert_eq!(ui.get_files().iter().next().unwrap().new_name, "02.txt");
    }

    #[test]
    fn pad_width_uses_the_sum_of_start_and_count() {
        let many: Vec<String> = (1..=40).map(|i| format!("/t/f{i:02}.txt")).collect();
        let (ui, _state) = seeded(&many.iter().map(String::as_str).collect::<Vec<_>>());
        ui.set_start_text("3".into());
        ui.set_new_kind("pattern".into());
        ui.set_field_a("<num>.<ext>".into());
        ui.invoke_add_rule();
        // 3 + 40 - 1 = 42, two digits. A product would give three.
        assert_eq!(ui.get_files().iter().next().unwrap().new_name, "03.txt");
    }

    #[test]
    fn copy_and_move_modes_use_the_destination_rename_ignores_it() {
        let (ui, state) = seeded(&["/t/a.txt"]);
        ui.set_dest_text("/dest".into());
        ui.set_new_kind("case".into());
        ui.set_case_mode("upper".into());
        ui.invoke_add_rule();
        // Rename ignores dest entirely and shows a bare name.
        let shown = ui.get_files().iter().next().unwrap().new_name.to_string();
        assert_eq!(shown, "A.TXT");

        for mode in ["copy", "move"] {
            ui.set_batch_mode(mode.into());
            preview::refresh(&ui, &state.borrow());
            let shown = ui.get_files().iter().next().unwrap().new_name.to_string();
            assert!(shown.contains("dest"), "{mode} showed {shown}");
            assert!(shown.ends_with("A.TXT"), "{mode} showed {shown}");
        }
    }

    #[test]
    fn rows_count_changes_errors_and_manual_overrides() {
        let (ui, state) = seeded(&["/t/a.txt", "/t/b.txt"]);
        // Both collapse onto one name, and Fail is the default policy.
        ui.set_new_kind("pattern".into());
        ui.set_field_a("same.<ext>".into());
        ui.invoke_add_rule();
        assert_eq!((ui.get_changed(), ui.get_errors()), (1, 1));
        let rows: Vec<_> = ui.get_files().iter().collect();
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].state, 1);
        assert_eq!(rows[1].state, 2, "second is a duplicate target");

        // A manual override on a changed row is labelled as such.
        state
            .borrow_mut()
            .overrides
            .insert(PathBuf::from("/t/a.txt"), "manual.txt".into());
        preview::refresh(&ui, &state.borrow());
        assert_eq!(ui.get_files().iter().next().unwrap().status, "manual");
    }

    #[test]
    fn search_filters_on_both_the_old_and_the_new_name() {
        let (ui, state) = seeded(&["/t/alpha.txt", "/t/beta.txt"]);
        ui.set_new_kind("replace".into());
        ui.set_field_a("beta".into());
        ui.set_field_b("gamma".into());
        ui.invoke_add_rule();
        assert_eq!(ui.get_files().iter().count(), 2);

        // "alpha" only appears in an old name...
        ui.set_search_text("alpha".into());
        preview::refresh(&ui, &state.borrow());
        assert_eq!(ui.get_files().iter().count(), 1);

        // ...and "gamma" only in a new one. Both must match.
        ui.set_search_text("gamma".into());
        preview::refresh(&ui, &state.borrow());
        let hit = ui.get_files().iter().next().unwrap();
        assert_eq!(hit.old_name, "beta.txt");

        ui.set_search_text("nothing".into());
        preview::refresh(&ui, &state.borrow());
        assert_eq!(ui.get_files().iter().count(), 0);
    }

    #[test]
    fn the_rule_stack_labels_list_replace_specially() {
        let (ui, state) = seeded(&["/t/a.txt"]);
        for (kind, a, b) in [("replace", "x", "y"), ("pairs", "x=y", "")] {
            state.borrow_mut().rules.push(model::RuleSpec {
                kind: kind.into(),
                a: a.into(),
                b: b.into(),
                mods: String::new(),
            });
        }
        preview::refresh(&ui, &state.borrow());
        let rows: Vec<_> = ui.get_rules().iter().collect();
        assert_eq!(rows[0].kind, "REPLACE");
        // "pairs" is shown under its user-facing name, not its internal one.
        assert_eq!(rows[1].kind, "LIST REPLACE");
        assert_eq!(rows[1].summary, "1 pair(s)");
    }
}
