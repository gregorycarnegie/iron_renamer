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
