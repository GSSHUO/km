#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod quota;
mod settings;
mod windowing;

use quota::QuotaState;
use serde_json::json;
use settings::{Settings, SettingsState};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{
    menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, WindowEvent,
};
use windowing::DockEdge;

struct TrayChecks {
    themes: Vec<tauri::menu::CheckMenuItem<tauri::Wry>>,
    metrics: Vec<tauri::menu::CheckMenuItem<tauri::Wry>>,
    motions: Vec<tauri::menu::CheckMenuItem<tauri::Wry>>,
    custom: tauri::menu::CheckMenuItem<tauri::Wry>,
}
static TRAY_CHECKS: std::sync::OnceLock<TrayChecks> = std::sync::OnceLock::new();

fn sync_tray(settings: &Settings) {
    let Some(checks) = TRAY_CHECKS.get() else {
        return;
    };
    for item in &checks.themes {
        let id = item.id().as_ref();
        let _ = item.set_checked(id == format!("theme-{}", settings.theme.selected_id));
    }
    let custom_active =
        !settings::preset_ids(&serde_json::from_str(settings::THEME_PRESETS).unwrap_or(json!([])))
            .contains(&settings.theme.selected_id);
    let _ = checks.custom.set_checked(custom_active);
    for item in &checks.metrics {
        let _ = item.set_checked(item.id().as_ref() == format!("metric-{}", settings.orb.metric));
    }
    for item in &checks.motions {
        let _ =
            item.set_checked(item.id().as_ref() == format!("motion-{}", settings.motion.preset));
    }
}

fn publish_settings(app: &AppHandle, config: serde_json::Value) {
    if let Ok(settings) = serde_json::from_value::<Settings>(config["settings"].clone()) {
        sync_tray(&settings);
    }
    if let Some(window) = app.get_webview_window("main") {
        windowing::reapply_material(&window);
    }
    let _ = app.emit("settings-changed", config);
}

fn commit_settings(app: &AppHandle, next: Settings, expected_revision: u64) -> serde_json::Value {
    let state = app.state::<SettingsState>();
    let result = {
        let mut store = state.0.lock().unwrap_or_else(|e| e.into_inner());
        store.commit(next, expected_revision)
    };
    match result {
        Ok(payload) => {
            let config = {
                let store = state.0.lock().unwrap_or_else(|e| e.into_inner());
                store.config()
            };
            publish_settings(app, config);
            payload
        }
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn report_dom(state: String) {
    println!("dom-report: {state}");
}

#[tauri::command]
fn expand_card(window: tauri::WebviewWindow) {
    windowing::exit_ball(&window);
}

#[tauri::command]
fn dock_ball(window: tauri::WebviewWindow) {
    if windowing::in_ball_mode() {
        return;
    }
    let edge = match (
        window.outer_position(),
        window.outer_size(),
        window.current_monitor(),
    ) {
        (Ok(pos), Ok(size), Ok(Some(monitor))) => {
            let center = pos.x + size.width as i32 / 2;
            if center < monitor.position().x + monitor.size().width as i32 / 2 {
                DockEdge::Left
            } else {
                DockEdge::Right
            }
        }
        _ => DockEdge::Right,
    };
    windowing::enter_ball(&window, edge);
}

fn clean_error(message: &str) -> String {
    quota::clean_error(message)
}

fn ok_or_err(result: anyhow::Result<serde_json::Value>) -> serde_json::Value {
    match result {
        Ok(value) => value,
        Err(error) => json!({ "ok": false, "error": clean_error(&error.to_string()) }),
    }
}

#[tauri::command]
fn get_ui_config(state: tauri::State<'_, SettingsState>) -> serde_json::Value {
    state.0.lock().unwrap_or_else(|e| e.into_inner()).config()
}

#[tauri::command]
fn migrate_settings(
    theme: Option<String>,
    metric: Option<String>,
    showcase: Option<bool>,
    app: AppHandle,
) -> serde_json::Value {
    let state = app.state::<SettingsState>();
    let result = {
        let mut store = state.0.lock().unwrap_or_else(|e| e.into_inner());
        store.migrate(theme, metric, showcase)
    };
    match result {
        Ok(payload) => {
            let config = state.0.lock().unwrap_or_else(|e| e.into_inner()).config();
            publish_settings(&app, config);
            payload
        }
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn save_settings(
    settings: serde_json::Value,
    expected_revision: u64,
    app: AppHandle,
) -> serde_json::Value {
    match settings::settings_from_value(settings) {
        Ok(settings) => commit_settings(&app, settings, expected_revision),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
async fn get_quota(app: AppHandle) -> serde_json::Value {
    let state = app.state::<QuotaState>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.fetch())
        .await
        .unwrap_or_else(|error| json!({ "ok": false, "error": error.to_string() }))
}

#[tauri::command]
fn start_drag(window: tauri::Window) {
    windowing::note_drag_started();
    let _ = window.start_dragging();
}

#[tauri::command]
fn session_status() -> serde_json::Value {
    auth::session_status()
}

#[tauri::command]
fn reset_session_state(state: tauri::State<'_, QuotaState>) -> serde_json::Value {
    json!({ "ok": true, "sessionGeneration": state.reset_session() })
}

#[tauri::command]
fn set_panel_layout(layout: String, window: tauri::WebviewWindow) {
    windowing::set_panel_layout(&window, &layout);
}

#[tauri::command]
fn hide_card(window: tauri::WebviewWindow) {
    windowing::hide_window(&window);
}

fn diagnostics_payload(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    quota: &QuotaState,
    settings: &SettingsState,
) -> serde_json::Value {
    let store = settings.0.lock().unwrap_or_else(|e| e.into_inner());
    json!({
        "appVersion": app.package_info().version.to_string(),
        "system": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "webViewVersion": "无法可靠获取",
        "quota": quota.diagnostics(),
        "window": windowing::diagnostics(window),
        "theme": store.settings.theme.selected_id,
        "motion": store.settings.motion.preset,
        "materialPreference": store.settings.material.mode,
    })
}

#[tauri::command]
fn get_diagnostics(
    app: AppHandle,
    window: tauri::WebviewWindow,
    quota: tauri::State<'_, QuotaState>,
    settings: tauri::State<'_, SettingsState>,
) -> serde_json::Value {
    diagnostics_payload(&app, &window, &quota, &settings)
}

fn write_clipboard(app: &AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard()
        .write_text(text)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn copy_quota_summary(app: AppHandle, quota: tauri::State<'_, QuotaState>) -> serde_json::Value {
    let result = quota.summary().and_then(|text| write_clipboard(&app, text));
    match result {
        Ok(_) => json!({ "ok": true }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn copy_diagnostics(
    app: AppHandle,
    window: tauri::WebviewWindow,
    quota: tauri::State<'_, QuotaState>,
    settings: tauri::State<'_, SettingsState>,
) -> serde_json::Value {
    let payload = diagnostics_payload(&app, &window, &quota, &settings);
    match write_clipboard(
        &app,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    ) {
        Ok(_) => json!({ "ok": true }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn copy_theme_text(text: String, app: AppHandle) -> serde_json::Value {
    let result = settings::merged_theme_text(&text)
        .and_then(|theme| write_clipboard(&app, settings::export_theme(&theme)));
    match result {
        Ok(_) => json!({ "ok": true }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

// ---------- embedded web login ----------
//
// The official login page runs on the real kimi.com origin inside this
// window, so its captcha and SMS flow work untouched. The page cannot call
// back into Tauri, so we poll its localStorage with a host-side eval; once
// tokens appear the script navigates to a fake callback URL which we
// intercept and cancel — tokens travel only inside this process.
//
// Self-healing: a one-shot watchdog probes the page state after 15 s via a
// fake `__kqb_diag__` navigation (also host-intercepted). Still blank → swap
// in a LOCAL error page with retry/close buttons; probe unanswered → the
// renderer is hung, so the window is destroyed and recreated on the error
// page. The user is never trapped in a white, unclosable window.

const WEB_LOGIN_LABEL: &str = "web-login";
const WEB_LOGIN_HOME: &str = "https://www.kimi.com/";

const WEB_LOGIN_POLL_JS: &str = r#"(function(){try{
if(!window.__kqbErr){window.__kqbErr=[];window.addEventListener('error',function(e){try{if(window.__kqbErr.length<20)window.__kqbErr.push(String((e&&e.message)||(e&&e.type)||'err'));}catch(_){}});window.addEventListener('unhandledrejection',function(e){try{if(window.__kqbErr.length<20)window.__kqbErr.push('rej:'+String(e&&e.reason));}catch(_){}});}
var at=localStorage.getItem('access_token'),rt=localStorage.getItem('refresh_token');
if(at&&rt){var uid=localStorage.getItem('msh_user_id')||'';location.href='https://www.kimi.com/__kqb_login_callback__?at='+encodeURIComponent(at)+'&rt='+encodeURIComponent(rt)+'&uid='+encodeURIComponent(uid);}
}catch(e){}})();"#;

/// One-shot probe: reports readyState / body size / collected JS errors by
/// navigating to a fake URL the host intercepts (and cancels).
const WEB_LOGIN_DIAG_JS: &str = r#"(function(){try{
var bl=document.body?document.body.innerHTML.length:-1;
var ch=document.body?document.body.childElementCount:0;
var q='rs='+encodeURIComponent(document.readyState)+'&bl='+bl+'&ch='+ch+'&t='+encodeURIComponent(document.title||'')+'&err='+encodeURIComponent((window.__kqbErr||[]).slice(0,5).join('|'));
location.href='https://www.kimi.com/__kqb_diag__?'+q;
}catch(e){}})();"#;

static WEBLOGIN_DIAG_RECEIVED: AtomicBool = AtomicBool::new(false);
static WEBLOGIN_DIAG_PRINT: AtomicBool = AtomicBool::new(false);
/// Generation guard so retried logins never end up with duplicate poll loops.
static WEBLOGIN_POLL_GEN: AtomicU64 = AtomicU64::new(0);

fn append_diag_log(msg: &str) {
    let Some(dir) = dirs::config_dir() else {
        return;
    };
    let p = dir.join("kimi-quota-bar").join("web-login-diag.log");
    use std::io::Write;
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        let _ = writeln!(f, "{msg}");
    }
}

/// Handles both fake URLs: `__kqb_login_callback__` (tokens) and
/// `__kqb_diag__` (page-state probe). Everything else navigates freely.
fn web_login_nav_handler(app: &AppHandle, url: &tauri::Url) -> bool {
    match url.path() {
        "/__kqb_diag__" => {
            WEBLOGIN_DIAG_RECEIVED.store(true, Ordering::SeqCst);
            let mut body_len: i64 = -1;
            for (k, v) in url.query_pairs() {
                if k == "bl" {
                    body_len = v.parse().unwrap_or(-1);
                }
            }
            let msg = format!("web-login diag: {}", url.query().unwrap_or(""));
            log::warn!("{msg}");
            append_diag_log(&msg);
            if WEBLOGIN_DIAG_PRINT.load(Ordering::SeqCst) {
                println!("{msg}");
            }
            // Page stayed (near-)empty 15 s in → rebuild the window on the
            // local error page (App URL is scheme-agnostic: http in debug,
            // https in release).
            if body_len < 200 {
                append_diag_log("web-login page blank; switching to error page");
                if let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) {
                    let _ = w.close();
                }
                build_web_login_window(app, tauri::WebviewUrl::App("weblogin-error.html".into()));
            }
            false
        }
        "/__kqb_login_callback__" => {
            let mut at = None;
            let mut rt = None;
            let mut uid = String::new();
            for (k, v) in url.query_pairs() {
                match k.as_ref() {
                    "at" => at = Some(v.into_owned()),
                    "rt" => rt = Some(v.into_owned()),
                    "uid" => uid = v.into_owned(),
                    _ => {}
                }
            }
            if let (Some(at), Some(rt)) = (at, rt) {
                if let Err(e) = auth::save_session(&serde_json::json!({
                    "access_token": at,
                    "refresh_token": rt,
                    "user_id": uid,
                    "source": "web",
                })) {
                    log::warn!("网页登录会话保存失败: {e}");
                }
            }
            notify_session_changed(app);
            if let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) {
                let _ = w.close();
            }
            if let Some(m) = app.get_webview_window("main") {
                let _ = m.eval(
                    "window.__kqbPoll && window.__kqbPoll(true); window.__kqbSession && window.__kqbSession()",
                );
            }
            false
        }
        _ => true,
    }
}

fn build_web_login_window(app: &AppHandle, url: tauri::WebviewUrl) {
    let nav_app = app.clone();
    let builder = tauri::WebviewWindowBuilder::new(app, WEB_LOGIN_LABEL, url)
        .title("登录 Kimi 账号")
        .inner_size(420.0, 680.0)
        .center()
        .on_navigation(move |url| web_login_nav_handler(&nav_app, url));
    if let Err(e) = builder.build() {
        log::warn!("无法创建网页登录窗口: {e}");
        append_diag_log(&format!("build failed: {e}"));
    }
}

/// Poll the page for tokens every 1.5 s; exits when the window is gone or a
/// newer poll loop supersedes this one.
fn start_web_login_poll(app: AppHandle) {
    let gen = WEBLOGIN_POLL_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(1500));
        if WEBLOGIN_POLL_GEN.load(Ordering::SeqCst) != gen {
            break;
        }
        match app.get_webview_window(WEB_LOGIN_LABEL) {
            Some(w) => {
                let _ = w.eval(WEB_LOGIN_POLL_JS);
            }
            None => break,
        }
    });
}

/// Record the login window's URL a few times after creation so any failed
/// run leaves a complete timeline in the diag log.
fn start_web_login_url_timeline(app: AppHandle) {
    std::thread::spawn(move || {
        let mut elapsed = 0u64;
        for target in [3u64, 8, 15, 25] {
            std::thread::sleep(Duration::from_secs(target - elapsed));
            elapsed = target;
            let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) else {
                return;
            };
            let url = w
                .url()
                .map(|u| u.to_string())
                .unwrap_or_else(|_| "<url error>".into());
            append_diag_log(&format!("web-login timeline t={target}s url={url}"));
        }
    });
}

/// Destroy the login window and rebuild it on the local error page (main
/// thread hop required for WebView2).
fn recreate_web_login_on_error_page(app: &AppHandle) {
    let app2 = app.clone();
    let dispatcher = app.clone();
    let _ = dispatcher.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window(WEB_LOGIN_LABEL) {
            let _ = w.close();
        }
        build_web_login_window(&app2, tauri::WebviewUrl::App("weblogin-error.html".into()));
    });
}

/// Force the compositor to repaint: a live DOM can still paint white when
/// WebView2's GPU compositor freezes (observed in the field: healthy page
/// per the diag probe, white window on screen). A 1-px size nudge kicks
/// the compositor without disturbing the layout.
fn nudge_web_login_repaint(app: &AppHandle) {
    let app2 = app.clone();
    let dispatcher = app.clone();
    let _ = dispatcher.run_on_main_thread(move || {
        let Some(w) = app2.get_webview_window(WEB_LOGIN_LABEL) else {
            return;
        };
        if let Ok(size) = w.outer_size() {
            let _ = w.set_size(tauri::PhysicalSize::new(size.width + 1, size.height));
            let _ = w.set_size(size);
        }
    });
    append_diag_log("web-login repaint nudged");
}

/// One-shot blank/hang watchdog (re-armed on every retry).
fn start_web_login_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(15));
        let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) else {
            return;
        };
        let url_now = w
            .url()
            .map(|u| u.to_string())
            .unwrap_or_else(|_| "<url error>".into());
        append_diag_log(&format!("web-login watchdog check: url={url_now}"));
        let on_kimi = w
            .url()
            .map(|u| u.host_str() == Some("www.kimi.com"))
            .unwrap_or(false);
        if !on_kimi {
            // Navigation never committed (stuck on about:blank) or went
            // somewhere unexpected: force one reload, then re-check.
            append_diag_log("web-login not on kimi.com; forcing reload");
            let _ = w.eval(&format!("location.replace('{WEB_LOGIN_HOME}')"));
            std::thread::sleep(Duration::from_secs(15));
            let Some(w2) = app.get_webview_window(WEB_LOGIN_LABEL) else {
                return;
            };
            let url2 = w2
                .url()
                .map(|u| u.to_string())
                .unwrap_or_else(|_| "<url error>".into());
            append_diag_log(&format!("web-login recheck after reload: url={url2}"));
            let on_kimi2 = w2
                .url()
                .map(|u| u.host_str() == Some("www.kimi.com"))
                .unwrap_or(false);
            if !on_kimi2 {
                append_diag_log("web-login still off kimi.com after reload; recreating window");
                recreate_web_login_on_error_page(&app);
                return;
            }
            // Reload landed on kimi.com — fall through to the probe below.
            WEBLOGIN_DIAG_RECEIVED.store(false, Ordering::SeqCst);
            let _ = w2.eval(WEB_LOGIN_DIAG_JS);
            std::thread::sleep(Duration::from_secs(3));
            if WEBLOGIN_DIAG_RECEIVED.load(Ordering::SeqCst) {
                nudge_web_login_repaint(&app);
                return;
            }
            append_diag_log("web-login diag probe unanswered after reload; recreating window");
            recreate_web_login_on_error_page(&app);
            return;
        }
        WEBLOGIN_DIAG_RECEIVED.store(false, Ordering::SeqCst);
        let _ = w.eval(WEB_LOGIN_DIAG_JS);
        std::thread::sleep(Duration::from_secs(3));
        if WEBLOGIN_DIAG_RECEIVED.load(Ordering::SeqCst) {
            // Page is healthy in the DOM (the nav handler already rebuilt a
            // truly blank page onto the error page) — kick the compositor
            // so a frozen frame actually paints.
            nudge_web_login_repaint(&app);
            return;
        }
        // Renderer hung — destroy and recreate on the local error page.
        append_diag_log("web-login diag probe unanswered; recreating window");
        recreate_web_login_on_error_page(&app);
    });
}

#[tauri::command]
fn open_web_login(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    // Building a WebviewWindow off the async command thread can leave
    // WebView2 uninitialized on Windows (white, unresponsive window) — hop
    // onto the main event loop first.
    let dispatcher = app.clone();
    let _ = dispatcher.run_on_main_thread(move || {
        build_web_login_window(
            &app,
            tauri::WebviewUrl::External(WEB_LOGIN_HOME.parse().unwrap()),
        );
        start_web_login_poll(app.clone());
        start_web_login_watchdog(app.clone());
        start_web_login_url_timeline(app.clone());
    });
}

/// Local error page's retry button: reload kimi.com and re-arm poll+watchdog.
#[tauri::command]
fn retry_web_login(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) {
        let _ = w.eval(&format!("location.replace('{WEB_LOGIN_HOME}')"));
    }
    start_web_login_poll(app.clone());
    start_web_login_watchdog(app.clone());
    start_web_login_url_timeline(app);
}

/// Local error page's close button (in-page, always responsive).
#[tauri::command]
fn close_web_login(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) {
        let _ = w.close();
    }
}

/// Create a QR login ticket and render the QR matrix natively, so the
/// frontend needs no third-party JS library.
#[tauri::command]
async fn login_qr_create() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(|| {
        ok_or_err(auth::login_qr_create().and_then(|v| {
            let url = v["url"].as_str().unwrap_or_default().to_string();
            let qr = qrcode::QrCode::new(url.as_bytes())?;
            let w = qr.width();
            let colors = qr.to_colors();
            let mut rows = String::with_capacity(w * w + w);
            for y in 0..w {
                for x in 0..w {
                    rows.push(if colors[y * w + x] == qrcode::Color::Dark {
                        '1'
                    } else {
                        '0'
                    });
                }
                rows.push('\n');
            }
            Ok(json!({
                "ok": true,
                "code": v["code"],
                "url": url,
                "size": w,
                "matrix": rows,
            }))
        }))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))
}

fn notify_session_changed(app: &AppHandle) {
    let generation = app.state::<QuotaState>().reset_session();
    let _ = app.emit(
        "session-changed",
        json!({ "sessionGeneration": generation }),
    );
}

#[tauri::command]
async fn login_qr_poll(code: String, app: AppHandle) -> serde_json::Value {
    let result = tauri::async_runtime::spawn_blocking(move || {
        ok_or_err(auth::login_qr_poll(&code).map(|mut v| {
            v["ok"] = json!(true);
            v
        }))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
    if result["status"] == "STATUS_SUCCESS" {
        notify_session_changed(&app);
    }
    result
}

#[tauri::command]
async fn login_sms_send(country_code: String, number: String) -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(move || {
        ok_or_err(auth::login_sms_send(&country_code, &number).map(|_| json!({ "ok": true })))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))
}

#[tauri::command]
async fn login_sms_verify(
    country_code: String,
    number: String,
    verify_code: String,
    app: AppHandle,
) -> serde_json::Value {
    let result = tauri::async_runtime::spawn_blocking(move || {
        ok_or_err(
            auth::login_sms_verify(&country_code, &number, &verify_code)
                .map(|_| json!({ "ok": true })),
        )
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
    if result["ok"] == true {
        notify_session_changed(&app);
    }
    result
}

#[tauri::command]
async fn logout(app: AppHandle) -> serde_json::Value {
    let result = tauri::async_runtime::spawn_blocking(|| ok_or_err(auth::logout_any()))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
    if result["ok"] != false {
        notify_session_changed(&app);
    }
    result
}

/// Personal-center payload: session state, user id, best-effort membership.
#[tauri::command]
async fn get_profile() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(|| {
        let mut out = auth::session_status();
        if out["userId"].is_null() {
            out["userId"] = auth::current_user_id()
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null);
        }
        match auth::fetch_membership_summary() {
            Ok(m) => out["membership"] = m,
            Err(e) => out["membershipError"] = json!(clean_error(&e.to_string())),
        }
        out["ok"] = json!(true);
        out
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))
}

// ---------- window show/hide ----------

fn handle_tray_selection(app: &AppHandle, id: &str) {
    let state = app.state::<SettingsState>();
    let (mut next, revision) = {
        let store = state.0.lock().unwrap_or_else(|e| e.into_inner());
        (store.settings.clone(), store.revision)
    };
    if let Some(theme) = id.strip_prefix("theme-") {
        next.theme.selected_id = theme.to_string();
    } else if let Some(metric) = id.strip_prefix("metric-") {
        next.orb.metric = metric.to_string();
    } else if let Some(motion) = id.strip_prefix("motion-") {
        next.motion.preset = motion.to_string();
    } else {
        return;
    }
    let result = commit_settings(app, next, revision);
    if result["ok"] != true {
        log::warn!("托盘设置保存失败: {}", result["error"]);
    }
}

// ---------- main ----------

fn main() {
    env_logger::init();

    // WebView2 honors the system proxy; a stale one (e.g. a closed local
    // capture tool on 127.0.0.1) leaves external pages like the web-login
    // window blank and the renderer hung. Our own HTTP never uses a proxy,
    // so bypass it for the embedded browser too.
    #[cfg(target_os = "windows")]
    std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--no-proxy-server");

    // Hidden self-check entry: `--check` prints one quota fetch as JSON and
    // exits; `--check-refresh` forces a token renewal first. Handy for
    // verifying the auth chain without launching the UI.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check-settings") {
        let presets: serde_json::Value =
            serde_json::from_str(settings::THEME_PRESETS).unwrap_or(json!([]));
        let defaults = Settings::default();
        let result = settings::validate(&presets, &defaults);
        println!(
            "{}",
            json!({ "ok": result.is_ok(), "presets": presets.as_array().map_or(0, Vec::len), "defaultSettings": defaults, "error": result.err() })
        );
        return;
    }
    if args.iter().any(|a| a == "--check-layout") {
        println!(
            "{}",
            json!({ "ok": true, "card": { "width": windowing::CARD_WIDTH, "height": windowing::CARD_HEIGHT }, "ball": windowing::BALL_SIZE, "panels": { "login": 400, "profile": 320, "appearance": 460, "diagnostics": 400 } })
        );
        return;
    }
    let self_check = args.iter().any(|a| {
        a == "--check"
            || a == "--check-refresh"
            || a == "--check-login"
            || a == "--check-session"
            || a == "--check-qr"
            || a == "--check-profile"
    });
    if self_check {
        // Release builds are GUI-subsystem on Windows: stdout goes nowhere
        // unless we first borrow the parent terminal's console. Harmless
        // no-op when there is no parent console (double-click launch).
        #[cfg(target_os = "windows")]
        unsafe {
            use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        if args.iter().any(|a| a == "--check-profile") {
            // Personal-center data path: membership summary + user id.
            let out = match auth::fetch_membership_summary() {
                Ok(mut v) => {
                    v["ok"] = json!(true);
                    v["userId"] = auth::current_user_id()
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null);
                    v
                }
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            };
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            return;
        }
        if args.iter().any(|a| a == "--check-qr") {
            // Full QR-login chain minus persistence: create a ticket,
            // confirm it with the current account token (exactly what the
            // phone app does after a scan), then verify the poll parses
            // the camelCase token fields.
            let out = match auth::check_qr_login() {
                Ok(v) => v,
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            };
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            return;
        }
        if args.iter().any(|a| a == "--check-session") {
            // Round-trips the session crypto (DPAPI on Windows) without
            // touching the real session file.
            let out = match auth::session_crypto_roundtrip("{\"t\":\"kqb-probe\"}") {
                Ok(true) => json!({ "ok": true }),
                Ok(false) => json!({ "ok": false, "error": "crypto roundtrip mismatch" }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            };
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            return;
        }
        if args.iter().any(|a| a == "--check-login") {
            // Exercises the QR-login chain: ticket creation + native QR render.
            let out = match auth::login_qr_create() {
                Ok(v) => {
                    let url = v["url"].as_str().unwrap_or_default().to_string();
                    let size = qrcode::QrCode::new(url.as_bytes())
                        .map(|q| q.width())
                        .unwrap_or(0);
                    json!({ "ok": true, "ticket": v["code"], "url": url, "qr_size": size })
                }
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            };
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            return;
        }
        let force = args.iter().any(|a| a == "--check-refresh");
        let out = match auth::get_valid_access_token(force).and_then(|t| auth::fetch_quota(&t)) {
            Ok(raw) => quota::normalize(&raw),
            Err(e) => json!({ "ok": false, "error": clean_error(&e.to_string()) }),
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }

    let test_weblogin = args.iter().any(|a| a == "--check-weblogin");
    let test_weblogin_late = args.iter().any(|a| a == "--check-weblogin-late");
    if test_weblogin || test_weblogin_late {
        WEBLOGIN_DIAG_PRINT.store(true, Ordering::SeqCst);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_liquid_glass::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(SettingsState::load())
        .manage(QuotaState::new())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let window = app.get_webview_window("main").expect("main window missing");
            if let Some((x, y)) = windowing::initial_position(&window) {
                let _ = window.set_position(PhysicalPosition::new(x, y));
            }
            let app_handle = app.handle().clone();
            window.on_window_event(move |event| match event {
                WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    windowing::schedule_moved(&app_handle);
                }
                WindowEvent::CloseRequested { api, .. } => api.prevent_close(),
                _ => {}
            });

            let refresh_item = MenuItemBuilder::with_id("refresh", "立即刷新").build(app)?;
            let presets: serde_json::Value = serde_json::from_str(settings::THEME_PRESETS)?;
            let mut theme_items = Vec::new();
            let mut theme_menu = SubmenuBuilder::with_id(app, "theme", "外观主题");
            for preset in presets.as_array().into_iter().flatten() {
                let id = preset["id"].as_str().unwrap_or("abyss");
                let item = CheckMenuItemBuilder::with_id(
                    format!("theme-{id}"),
                    preset["label"].as_str().unwrap_or(id),
                )
                .build(app)?;
                theme_menu = theme_menu.item(&item);
                theme_items.push(item);
            }
            let custom_item =
                CheckMenuItemBuilder::with_id("theme-custom-current", "自定义 · 当前主题")
                    .enabled(false)
                    .build(app)?;
            theme_menu = theme_menu.item(&custom_item);
            let theme_menu = theme_menu.build()?;

            let mut metric_items = Vec::new();
            let mut metric_menu = SubmenuBuilder::with_id(app, "metric", "浮珠显示");
            for (id, label) in [
                ("total", "月度额度"),
                ("5h", "5 小时窗口"),
                ("7d", "7 天窗口"),
            ] {
                let item =
                    CheckMenuItemBuilder::with_id(format!("metric-{id}"), label).build(app)?;
                metric_menu = metric_menu.item(&item);
                metric_items.push(item);
            }
            let metric_menu = metric_menu.build()?;

            let mut motion_items = Vec::new();
            let mut motion_menu = SubmenuBuilder::with_id(app, "motion", "动效");
            for (id, label) in [
                ("static", "静止"),
                ("lively", "灵动"),
                ("stardust", "星尘 · 观赏模式"),
            ] {
                let item =
                    CheckMenuItemBuilder::with_id(format!("motion-{id}"), label).build(app)?;
                motion_menu = motion_menu.item(&item);
                motion_items.push(item);
            }
            let motion_menu = motion_menu.build()?;
            let _ = TRAY_CHECKS.set(TrayChecks {
                themes: theme_items,
                metrics: metric_items,
                motions: motion_items,
                custom: custom_item,
            });
            let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[
                    &refresh_item,
                    &theme_menu,
                    &metric_menu,
                    &motion_menu,
                    &quit_item,
                ])
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
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.eval("window.__kqbPoll && window.__kqbPoll(true)");
                        }
                    }
                    "quit" => app.exit(0),
                    id => handle_tray_selection(app, id),
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        windowing::toggle_window(tray.app_handle());
                    }
                })
                .build(app)?;

            {
                let state = app.state::<SettingsState>();
                let store = state.0.lock().unwrap_or_else(|e| e.into_inner());
                sync_tray(&store.settings);
            }
            windowing::show_window(&window);

            // --check-weblogin: open the web-login window for real, let the
            // 15 s watchdog probe fire (prints page state), then report the
            // final URL — kimi.com means it rendered; the local error page
            // means the fallback kicked in.
            if test_weblogin {
                let app_handle = app.handle().clone();
                open_web_login(app_handle.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(25));
                    let url = app_handle
                        .get_webview_window(WEB_LOGIN_LABEL)
                        .and_then(|w| w.url().ok())
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "window-missing".to_string());
                    println!("{}", json!({ "webLoginUrl": url }));
                    app_handle.exit(0);
                });
            }

            // --check-weblogin-late: same probe, but the window is created
            // 5 s after startup — replicating the real button-click timing.
            if test_weblogin_late {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(5));
                    open_web_login(app_handle.clone());
                    std::thread::sleep(Duration::from_secs(30));
                    let url = app_handle
                        .get_webview_window(WEB_LOGIN_LABEL)
                        .and_then(|w| w.url().ok())
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "window-missing".to_string());
                    println!("{}", json!({ "webLoginLateUrl": url }));
                    app_handle.exit(0);
                });
            }

            // --check-weblogin-error: open the window directly on the local
            // error page to verify the fallback asset resolves and renders.
            if args.iter().any(|a| a == "--check-weblogin-error") {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    let app2 = app_handle.clone();
                    let dispatcher = app_handle.clone();
                    let _ = dispatcher.run_on_main_thread(move || {
                        build_web_login_window(
                            &app2,
                            tauri::WebviewUrl::App("weblogin-error.html".into()),
                        );
                    });
                    std::thread::sleep(Duration::from_secs(6));
                    let url = app_handle
                        .get_webview_window(WEB_LOGIN_LABEL)
                        .and_then(|w| w.url().ok())
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "window-missing".to_string());
                    println!("{}", json!({ "webLoginErrorUrl": url }));
                    app_handle.exit(0);
                });
            }

            // --check-ball: drives the dock-to-edge flow programmatically —
            // push the card PAST the right edge (negative gap, the case that
            // used to silently do nothing), expect ball mode; drag away,
            // expect expand; then the same across the left edge.
            if args.iter().any(|a| a == "--check-ball") {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(2));
                    let Some(w) = app_handle.get_webview_window("main") else {
                        println!("{}", json!({ "ok": false, "error": "main window missing" }));
                        app_handle.exit(1);
                        return;
                    };
                    let Some(m) = w.current_monitor().ok().flatten() else {
                        println!("{}", json!({ "ok": false, "error": "no monitor" }));
                        app_handle.exit(1);
                        return;
                    };
                    let m_left = m.position().x;
                    let m_right = m.position().x + m.size().width as i32;
                    let home = w.outer_position().ok();
                    let size = w.outer_size().unwrap_or_default();
                    let y = home.as_ref().map(|p| p.y).unwrap_or(100);
                    let mut out = json!({ "ok": true });
                    // 1) overshoot the right edge → ball
                    let _ =
                        w.set_position(PhysicalPosition::new(m_right - size.width as i32 + 40, y));
                    std::thread::sleep(Duration::from_millis(2500));
                    out["rightBall"] = json!(windowing::in_ball_mode());
                    // Read back the actual DOM state via a hidden command:
                    // confirms the ball-mode switch reached the frontend.
                    let _ = w.eval(concat!(
                        "window.__TAURI__.core.invoke('report_dom', { state:",
                        " document.body.className + '|' +",
                        " getComputedStyle(document.querySelector('.card')).display + '|' +",
                        " getComputedStyle(document.getElementById('ball')).display })"
                    ));
                    std::thread::sleep(Duration::from_millis(400));
                    // 2) drag away → expand
                    if let Ok(p) = w.outer_position() {
                        let _ = w.set_position(PhysicalPosition::new(p.x - 300, p.y));
                    }
                    std::thread::sleep(Duration::from_millis(2500));
                    out["rightExpand"] = json!(!windowing::in_ball_mode());
                    // 3) overshoot the left edge → ball
                    let _ = w.set_position(PhysicalPosition::new(m_left - 30, y));
                    std::thread::sleep(Duration::from_millis(2500));
                    out["leftBall"] = json!(windowing::in_ball_mode());
                    // 4) drag away → expand
                    if let Ok(p) = w.outer_position() {
                        let _ = w.set_position(PhysicalPosition::new(p.x + 300, p.y));
                    }
                    std::thread::sleep(Duration::from_millis(2500));
                    out["leftExpand"] = json!(!windowing::in_ball_mode());
                    if let Some(p) = home {
                        let _ = w.set_position(p);
                    }
                    out["ok"] = json!(
                        out["rightBall"] == true
                            && out["rightExpand"] == true
                            && out["leftBall"] == true
                            && out["leftExpand"] == true
                    );
                    println!("{out}");
                    app_handle.exit(0);
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_quota,
            get_ui_config,
            migrate_settings,
            save_settings,
            start_drag,
            session_status,
            reset_session_state,
            set_panel_layout,
            login_qr_create,
            login_qr_poll,
            login_sms_send,
            login_sms_verify,
            logout,
            hide_card,
            open_web_login,
            retry_web_login,
            close_web_login,
            expand_card,
            dock_ball,
            report_dom,
            get_profile,
            get_diagnostics,
            copy_quota_summary,
            copy_diagnostics,
            copy_theme_text,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kimi Quota Bar");
}
