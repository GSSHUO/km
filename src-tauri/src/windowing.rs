use crate::settings::{resolve_theme, SettingsState};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

pub const CARD_WIDTH: i32 = 280;
pub const CARD_HEIGHT: i32 = 252;
pub const BALL_SIZE: i32 = 60;
const EDGE_ENTER_GAP: f64 = 28.0;
const EDGE_EXIT_GAP: f64 = 48.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DockEdge {
    Left,
    Right,
}

static BALL_MODE: Mutex<bool> = Mutex::new(false);
static BALL_DOCK: Mutex<DockEdge> = Mutex::new(DockEdge::Right);
static PANEL: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new("quota".into()));
static VISIBLE: AtomicBool = AtomicBool::new(false);
static GEOMETRY_GEN: AtomicU64 = AtomicU64::new(0);
static MOVE_GEN: AtomicU64 = AtomicU64::new(0);
static DRAG_REQUESTED: AtomicBool = AtomicBool::new(false);
static SUPPRESS_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static MOVE_SIGNAL: std::sync::OnceLock<Arc<(Mutex<u64>, Condvar)>> = std::sync::OnceLock::new();
static MATERIAL_INFO: LazyLock<Mutex<Value>> =
    LazyLock::new(|| Mutex::new(json!({ "mode": "unknown" })));

pub fn in_ball_mode() -> bool {
    *BALL_MODE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn note_drag_started() {
    DRAG_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn panel() -> String {
    PANEL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn state_path() -> Option<std::path::PathBuf> {
    let dir = dirs::config_dir()?.join("kimi-quota-bar");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("window-state.json"))
}

fn load_saved_position() -> Option<(i32, i32)> {
    let raw = std::fs::read_to_string(state_path()?).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    Some((value["x"].as_i64()? as i32, value["y"].as_i64()? as i32))
}

pub fn save_position(x: i32, y: i32) {
    if in_ball_mode() {
        return;
    }
    if let Some(path) = state_path() {
        let _ = std::fs::write(path, json!({ "x": x, "y": y }).to_string());
    }
}

fn work_area(window: &WebviewWindow) -> Option<(i32, i32, i32, i32)> {
    let monitor = window.current_monitor().ok().flatten()?;
    let area = monitor.work_area();
    Some((
        area.position.x,
        area.position.y,
        area.size.width as i32,
        area.size.height as i32,
    ))
}

fn clamp_axis(value: i32, origin: i32, available: i32, size: i32) -> i32 {
    if size >= available {
        origin
    } else {
        value.clamp(origin, origin + available - size)
    }
}

fn clamp_geometry(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    area: (i32, i32, i32, i32),
) -> (i32, i32) {
    (
        clamp_axis(x, area.0, area.2, width),
        clamp_axis(y, area.1, area.3, height),
    )
}

fn notify(window: &WebviewWindow) {
    let form = if in_ball_mode() { "ball" } else { "card" };
    let generation = GEOMETRY_GEN.load(Ordering::SeqCst);
    let payload = json!({ "visible": VISIBLE.load(Ordering::SeqCst), "form": form, "panel": panel(), "generation": generation });
    let _ = window.emit("app-visibility", payload.clone());
    let _ = window.emit("form-changed", payload);
    let ball = in_ball_mode();
    let _ = window.emit("ball-mode", ball);
    let _ = window.eval(&format!(
        "window.__kqbBallMode && window.__kqbBallMode({ball})"
    ));
}

fn animate_window_geom(
    window: &WebviewWindow,
    from: (i32, i32, i32, i32),
    to: (i32, i32, i32, i32),
    duration_ms: u64,
) {
    DRAG_REQUESTED.store(false, Ordering::SeqCst);
    let generation = GEOMETRY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let window = window.clone();
    std::thread::spawn(move || {
        let steps = (duration_ms / 20).max(1) as i32;
        for index in 1..=steps {
            if GEOMETRY_GEN.load(Ordering::SeqCst) != generation {
                return;
            }
            let t = index as f64 / steps as f64;
            let eased = 1.0 - (1.0 - t).powi(3);
            let lerp = |a: i32, b: i32| (a as f64 + (b - a) as f64 * eased).round() as i32;
            let _ = window.set_size(PhysicalSize::new(
                lerp(from.2, to.2).max(1) as u32,
                lerp(from.3, to.3).max(1) as u32,
            ));
            let _ = window.set_position(PhysicalPosition::new(
                lerp(from.0, to.0),
                lerp(from.1, to.1),
            ));
            if index < steps {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if GEOMETRY_GEN.load(Ordering::SeqCst) == generation {
            let _ = window.set_size(PhysicalSize::new(to.2.max(1) as u32, to.3.max(1) as u32));
            let _ = window.set_position(PhysicalPosition::new(to.0, to.1));
        }
    });
}

fn current_geometry(window: &WebviewWindow) -> (i32, i32, i32, i32) {
    let position = window.outer_position().unwrap_or_default();
    let size = window.outer_size().unwrap_or_default();
    (
        position.x,
        position.y,
        size.width as i32,
        size.height as i32,
    )
}

pub fn apply_material(
    window: &WebviewWindow,
    form: &str,
    appearance: &str,
    mode: &str,
    background: &str,
) {
    let radius = if form == "ball" {
        BALL_SIZE as f64 / 2.0
    } else {
        18.0
    };
    let _ = window.set_theme(Some(if appearance == "light" {
        tauri::Theme::Light
    } else {
        tauri::Theme::Dark
    }));
    let _ = window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
    let mut info = json!({ "requested": mode, "appearance": appearance, "form": form });

    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_liquid_glass::{LiquidGlassConfig, LiquidGlassExt};
        use window_vibrancy::{
            apply_vibrancy, clear_vibrancy, NSVisualEffectMaterial, NSVisualEffectState,
        };
        let tint = if matches!(background, "#0D101B" | "#161620") {
            "#1A1A2673".to_string()
        } else if background.len() == 7 {
            format!(
                "{background}{}",
                if appearance == "light" { "40" } else { "60" }
            )
        } else {
            background.to_string()
        };
        let _ = clear_vibrancy(window);
        if mode == "matte" {
            let _ = window.liquid_glass().set_effect(
                window,
                LiquidGlassConfig {
                    enabled: false,
                    corner_radius: radius,
                    ..Default::default()
                },
            );
            info["mode"] = json!("css-matte");
        } else {
            let config = LiquidGlassConfig {
                enabled: true,
                corner_radius: radius,
                tint_color: Some(tint),
                ..Default::default()
            };
            match window.liquid_glass().set_effect(window, config) {
                Ok(_) => info["mode"] = json!("liquid-glass"),
                Err(error) => match apply_vibrancy(
                    window,
                    NSVisualEffectMaterial::Popover,
                    Some(NSVisualEffectState::Active),
                    Some(radius),
                ) {
                    Ok(_) => {
                        info["mode"] = json!("vibrancy");
                        info["fallbackReason"] = json!(error.to_string());
                    }
                    Err(fallback) => {
                        info["mode"] = json!("css-matte");
                        info["fallbackReason"] = json!(fallback.to_string());
                    }
                },
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let _ = window_vibrancy::clear_acrylic(window);
        if mode == "enhanced" && form != "ball" {
            let color = if appearance == "light" {
                (245, 240, 243, 245)
            } else {
                (13, 16, 27, 245)
            };
            match window_vibrancy::apply_acrylic(window, Some(color)) {
                Ok(_) => info["mode"] = json!("acrylic-enhanced"),
                Err(error) => {
                    info["mode"] = json!("css-matte");
                    info["fallbackReason"] = json!(error.to_string());
                }
            }
        } else {
            info["mode"] = json!("css-matte");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        info["mode"] = json!("css-matte");
    }
    *MATERIAL_INFO.lock().unwrap_or_else(|e| e.into_inner()) = info;
}

pub fn reapply_material(window: &WebviewWindow) {
    let app = window.app_handle();
    let state = app.state::<SettingsState>();
    let store = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let theme = resolve_theme(&store.config()["presets"], &store.settings);
    let form = if in_ball_mode() { "ball" } else { "card" };
    apply_material(
        window,
        form,
        theme["appearance"].as_str().unwrap_or("dark"),
        &store.settings.material.mode,
        theme["palette"]["background"].as_str().unwrap_or("#0D101B"),
    );
}

pub fn enter_ball(window: &WebviewWindow, edge: DockEdge) {
    if in_ball_mode() {
        return;
    }
    *BALL_MODE.lock().unwrap_or_else(|e| e.into_inner()) = true;
    *BALL_DOCK.lock().unwrap_or_else(|e| e.into_inner()) = edge;
    *PANEL.lock().unwrap_or_else(|e| e.into_inner()) = "quota".into();
    *SUPPRESS_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Instant::now() + Duration::from_millis(1000));
    reapply_material(window);
    notify(window);

    let scale = window.scale_factor().unwrap_or(1.0);
    let size = (BALL_SIZE as f64 * scale).round() as i32;
    let from = current_geometry(window);
    let area = work_area(window);
    let x = area
        .map(|a| {
            if edge == DockEdge::Right {
                a.0 + a.2 - size
            } else {
                a.0
            }
        })
        .unwrap_or(from.0);
    let (x, y) = area
        .map(|a| clamp_geometry(x, from.1, size, size, a))
        .unwrap_or((x, from.1));
    animate_window_geom(window, from, (x, y, size, size), 200);
}

pub fn exit_ball(window: &WebviewWindow) {
    if !in_ball_mode() {
        return;
    }
    let edge = *BALL_DOCK.lock().unwrap_or_else(|e| e.into_inner());
    *BALL_MODE.lock().unwrap_or_else(|e| e.into_inner()) = false;
    *PANEL.lock().unwrap_or_else(|e| e.into_inner()) = "quota".into();
    *SUPPRESS_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Instant::now() + Duration::from_millis(1000));
    reapply_material(window);
    notify(window);

    let scale = window.scale_factor().unwrap_or(1.0);
    let width = (CARD_WIDTH as f64 * scale).round() as i32;
    let height = (CARD_HEIGHT as f64 * scale).round() as i32;
    let from = current_geometry(window);
    let anchored_x = if edge == DockEdge::Right {
        from.0 + from.2 - width
    } else {
        from.0
    };
    let (x, y) = work_area(window)
        .map(|area| clamp_geometry(anchored_x, from.1, width, height, area))
        .unwrap_or((anchored_x, from.1));
    animate_window_geom(window, from, (x, y, width, height), 220);
}

pub fn set_panel_layout(window: &WebviewWindow, layout: &str) {
    let (logical_width, logical_height) = match layout {
        "login" => (280, 400),
        "profile" => (280, 320),
        "appearance" => (280, 460),
        "diagnostics" => (280, 400),
        _ => (CARD_WIDTH, CARD_HEIGHT),
    };
    *BALL_MODE.lock().unwrap_or_else(|e| e.into_inner()) = false;
    *PANEL.lock().unwrap_or_else(|e| e.into_inner()) = if layout == "quota" {
        "quota".into()
    } else {
        layout.into()
    };
    *SUPPRESS_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Instant::now() + Duration::from_millis(800));
    reapply_material(window);
    notify(window);

    let scale = window.scale_factor().unwrap_or(1.0);
    let width = (logical_width as f64 * scale).round() as i32;
    let height = (logical_height as f64 * scale).round() as i32;
    let from = current_geometry(window);
    let (x, y) = work_area(window)
        .map(|area| clamp_geometry(from.0, from.1, width, height, area))
        .unwrap_or((from.0, from.1));
    animate_window_geom(window, from, (x, y, width, height), 220);
}

pub fn hide_window(window: &WebviewWindow) {
    if !in_ball_mode() {
        if let Ok(position) = window.outer_position() {
            save_position(position.x, position.y);
        }
    }
    GEOMETRY_GEN.fetch_add(1, Ordering::SeqCst);
    VISIBLE.store(false, Ordering::SeqCst);
    let _ = window.hide();
    notify(window);
}

pub fn show_window(window: &WebviewWindow) {
    let _ = window.show();
    let _ = window.set_always_on_top(true);
    VISIBLE.store(true, Ordering::SeqCst);
    reapply_material(window);
    notify(window);
}

pub fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        hide_window(&window);
    } else {
        show_window(&window);
    }
}

pub fn initial_position(window: &WebviewWindow) -> Option<(i32, i32)> {
    *SUPPRESS_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Instant::now() + Duration::from_millis(1200));
    let scale = window
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or_else(|| window.scale_factor().unwrap_or(1.0));
    let width = (CARD_WIDTH as f64 * scale).round() as i32;
    let height = (CARD_HEIGHT as f64 * scale).round() as i32;
    let monitors = window.available_monitors().unwrap_or_default();
    if let Some((x, y)) = load_saved_position() {
        let margin = (48.0 * scale).round() as i32;
        let saved = monitors.iter().find_map(|monitor| {
            let area = monitor.work_area();
            let bounds = (
                area.position.x,
                area.position.y,
                area.size.width as i32,
                area.size.height as i32,
            );
            let (cx, cy) = clamp_geometry(x, y, width, height, bounds);
            if cx != x || cy != y {
                return None;
            }
            let safe_x = if x - bounds.0 < margin {
                bounds.0 + margin
            } else if bounds.0 + bounds.2 - (x + width) < margin {
                bounds.0 + bounds.2 - width - margin
            } else {
                x
            };
            Some(clamp_geometry(safe_x, y, width, height, bounds))
        });
        if saved.is_some() {
            return saved;
        }
    }
    let area = work_area(window).or_else(|| {
        window.primary_monitor().ok().flatten().map(|m| {
            let area = m.work_area();
            (
                area.position.x,
                area.position.y,
                area.size.width as i32,
                area.size.height as i32,
            )
        })
    })?;
    let margin = (48.0 * scale).round() as i32;
    Some(clamp_geometry(
        area.0 + area.2 - width - margin,
        area.1 + margin,
        width,
        height,
        area,
    ))
}

pub fn schedule_moved(app: &AppHandle) {
    let generation = MOVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let signal = MOVE_SIGNAL
        .get_or_init(|| Arc::new((Mutex::new(0), Condvar::new())))
        .clone();
    {
        let (lock, wake) = &*signal;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = generation;
        wake.notify_all();
    }
    static WORKER_STARTED: AtomicBool = AtomicBool::new(false);
    if WORKER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let (lock, wake) = &*signal;
        let mut seen = lock.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let current = *seen;
            let (next, timeout) = wake
                .wait_timeout(seen, Duration::from_millis(200))
                .unwrap_or_else(|e| e.into_inner());
            seen = next;
            if !timeout.timed_out() || *seen != current {
                continue;
            }
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || process_moved(&app2));
        }
    });
}

fn process_moved(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if !DRAG_REQUESTED.swap(false, Ordering::SeqCst) {
        return;
    }
    if SUPPRESS_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|until| Instant::now() < until)
    {
        return;
    }
    let from = current_geometry(&window);
    let Some(area) = work_area(&window) else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    if !in_ball_mode() {
        let (x, y) = clamp_geometry(from.0, from.1, from.2, from.3, area);
        if (x, y) != (from.0, from.1) {
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
        save_position(x, y);
        let right_gap = area.0 + area.2 - (from.0 + from.2);
        let left_gap = from.0 - area.0;
        if (right_gap as f64) <= EDGE_ENTER_GAP * scale {
            enter_ball(&window, DockEdge::Right);
        } else if (left_gap as f64) <= EDGE_ENTER_GAP * scale {
            enter_ball(&window, DockEdge::Left);
        }
    } else {
        let edge = *BALL_DOCK.lock().unwrap_or_else(|e| e.into_inner());
        let gap = if edge == DockEdge::Right {
            area.0 + area.2 - (from.0 + from.2)
        } else {
            from.0 - area.0
        };
        if gap.unsigned_abs() as f64 > EDGE_EXIT_GAP * scale {
            exit_ball(&window);
        }
    }
}

pub fn diagnostics(window: &WebviewWindow) -> Value {
    let scale = window.scale_factor().unwrap_or(1.0);
    let size = window.outer_size().unwrap_or_default();
    json!({
        "logicalSize": { "width": (size.width as f64 / scale).round(), "height": (size.height as f64 / scale).round() },
        "scaleFactor": scale,
        "form": if in_ball_mode() { "ball" } else { "card" },
        "panel": panel(),
        "visible": window.is_visible().unwrap_or(false),
        "material": MATERIAL_INFO.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_clamps_both_axes_and_handles_small_work_areas() {
        assert_eq!(
            clamp_geometry(900, 900, 280, 252, (0, 0, 1000, 700)),
            (720, 448)
        );
        assert_eq!(clamp_geometry(-20, -50, 500, 800, (0, 0, 400, 600)), (0, 0));
    }
}
