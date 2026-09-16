#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;

use serde_json::json;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, WindowEvent,
};

const CARD_WIDTH: i32 = 328;

// ---------- persistence ----------

fn state_path() -> Option<std::path::PathBuf> {
    let dir = dirs::config_dir()?.join("kimi-quota-bar");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("window-state.json"))
}

fn load_saved_position() -> Option<(i32, i32)> {
    let raw = std::fs::read_to_string(state_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((v["x"].as_i64()? as i32, v["y"].as_i64()? as i32))
}

fn save_position(x: i32, y: i32) {
    if let Some(p) = state_path() {
        let _ = std::fs::write(p, json!({ "x": x, "y": y }).to_string());
    }
}

// ---------- quota fetch ----------

fn pick<'a>(raw: &'a serde_json::Value, keys: &[&str]) -> Option<&'a serde_json::Value> {
    keys.iter().find_map(|k| raw.get(*k)).filter(|v| !v.is_null())
}

fn ratio_of(stat: Option<&serde_json::Value>, field: &str) -> Option<f64> {
    stat.and_then(|s| s.get(field)).and_then(|v| v.as_f64())
}

fn str_of(stat: Option<&serde_json::Value>, field: &str) -> Option<String> {
    stat.and_then(|s| s.get(field))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn normalize(raw: &serde_json::Value) -> serde_json::Value {
    let balance = pick(raw, &["subscriptionBalance"]);
    let five = pick(raw, &["ratelimitCode5h", "ratelimit5h"]);
    let seven = pick(raw, &["ratelimitCode7d", "ratelimit7d"]);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // proto3 JSON omits zero-valued fields: a missing ratio on a present
    // stat object means 0% used, not "unknown".
    let zero_if = |stat: Option<&serde_json::Value>| stat.map(|_| 0.0);
    json!({
        "ok": true,
        "fetchedAt": now,
        "total": {
            "usedRatio": ratio_of(balance, "amountUsedRatio").or(zero_if(balance)),
            "codeRatio": ratio_of(balance, "kimiCodeUsedRatio").or(zero_if(balance)),
            "resetTime": str_of(balance, "expireTime"),
        },
        "fiveHour": {
            "usedRatio": ratio_of(five, "ratio").or(zero_if(five)),
            "enabled": five.and_then(|s| s.get("enabled")).and_then(|v| v.as_bool()).unwrap_or(true),
            "resetTime": str_of(five, "resetTime"),
        },
        "sevenDay": {
            "usedRatio": ratio_of(seven, "ratio").or(zero_if(seven)),
            "enabled": seven.and_then(|s| s.get("enabled")).and_then(|v| v.as_bool()).unwrap_or(true),
            "resetTime": str_of(seven, "resetTime"),
        },
    })
}

fn fetch_payload() -> serde_json::Value {
    match auth::fetch_quota_smart() {
        Ok(raw) => normalize(&raw),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

// ---------- commands (invoked from the card UI) ----------

/// The frontend polls this on a timer — invoke is the reliable channel.
#[tauri::command]
async fn get_quota() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(fetch_payload)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))
}

#[tauri::command]
fn start_drag(window: tauri::Window) {
    let _ = window.start_dragging();
}

// ---------- window show/hide ----------

fn toggle_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    if visible {
        if let Ok(pos) = window.outer_position() {
            save_position(pos.x, pos.y);
        }
        let _ = window.hide();
    } else {
        let _ = window.show();
        let _ = window.set_always_on_top(true);
    }
}

// ---------- main ----------

fn main() {
    env_logger::init();

    // Hidden self-check entry: `--check` prints one quota fetch as JSON and
    // exits; `--check-refresh` forces a token renewal first. Handy for
    // verifying the auth chain without launching the UI.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check" || a == "--check-refresh") {
        let force = args.iter().any(|a| a == "--check-refresh");
        let out = match auth::get_valid_access_token(force)
            .and_then(|t| auth::fetch_quota(&t))
        {
            Ok(raw) => normalize(&raw),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_liquid_glass::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let window = app
                .get_webview_window("main")
                .expect("main window missing");

            // Force dark appearance so the vibrancy blur stays dark glass
            // even when the system is in light mode.
            let _ = window.set_theme(Some(tauri::Theme::Dark));

            // WKWebView defaults to an opaque WHITE background, which hides
            // the vibrancy layer and leaks white at the rounded corners.
            let _ = window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));

            // Native blur behind the transparent card.
            #[cfg(target_os = "macos")]
            {
                use tauri_plugin_liquid_glass::{LiquidGlassConfig, LiquidGlassExt};
                // macOS 26+: real Liquid Glass (NSGlassEffectView) with a dark
                // tint and rounded corners; older macOS: classic vibrancy.
                let glass_cfg = LiquidGlassConfig {
                    enabled: true,
                    corner_radius: 16.0,
                    tint_color: Some("#1A1A2673".to_string()),
                    ..Default::default()
                };
                if app.liquid_glass().set_effect(&window, glass_cfg).is_err() {
                    use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
                    let _ = apply_vibrancy(
                        &window,
                        NSVisualEffectMaterial::Popover,
                        Some(NSVisualEffectState::Active),
                        Some(16.0),
                    );
                }
            }
            #[cfg(target_os = "windows")]
            {
                let _ = window_vibrancy::apply_acrylic(&window, Some((16, 16, 24, 200)));
            }

            // Restore last position (clamped on-screen), else default top-right.
            let monitor = window.primary_monitor().ok().flatten();
            let fallback = monitor.as_ref().map(|m| {
                let size = m.size();
                let origin = m.position();
                (origin.x + size.width as i32 - CARD_WIDTH - 16, origin.y + 32)
            });
            let pos = load_saved_position()
                .filter(|(x, y)| {
                    monitor
                        .as_ref()
                        .map(|m| {
                            let size = m.size();
                            let origin = m.position();
                            *x >= origin.x - 50
                                && *y >= origin.y - 50
                                && *x + CARD_WIDTH <= origin.x + size.width as i32 + 50
                                && *y + 100 <= origin.y + size.height as i32 + 50
                        })
                        .unwrap_or(true)
                })
                .or(fallback);
            if let Some((x, y)) = pos {
                let _ = window.set_position(PhysicalPosition::new(x, y));
            }

            // Save position after dragging (debounced); never let the window
            // be destroyed — a menu-bar app must not exit on window close.
            let last_save = Mutex::new(Instant::now() - Duration::from_secs(5));
            window.on_window_event(move |event| match event {
                WindowEvent::Moved(pos) => {
                    let mut guard = last_save.lock().unwrap();
                    if guard.elapsed() > Duration::from_millis(400) {
                        *guard = Instant::now();
                        save_position(pos.x, pos.y);
                    }
                }
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                }
                _ => {}
            });

            // Tray: left click toggles the card; right click opens menu.
            let refresh_item = MenuItemBuilder::with_id("refresh", "立即刷新").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&refresh_item, &quit_item])
                .build()?;
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            TrayIconBuilder::with_id("main-tray")
                .icon(tray_icon)
                .icon_as_template(true)
                .tooltip("Kimi 套餐额度")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "refresh" => {
                        // Poke the card to re-poll immediately.
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.eval("window.__kqbPoll && window.__kqbPoll()");
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_window(tray.app_handle());
                    }
                })
                .build(app)?;

            // Show the card at launch.
            let _ = window.show();
            let _ = window.set_always_on_top(true);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_quota, start_drag])
        .run(tauri::generate_context!())
        .expect("error while running Kimi Quota Bar");
}
