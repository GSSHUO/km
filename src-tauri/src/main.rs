#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;

use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, PhysicalPosition, WindowEvent,
};

const CARD_WIDTH: i32 = 328;
const CARD_HEIGHT: i32 = 224;
/// Ball-mode diameter (logical px) and edge-proximity thresholds (physical
/// px). ENTER must stay well under EXIT so the snap itself never re-triggers.
const BALL_SIZE: f64 = 72.0;
const EDGE_ENTER_GAP: i32 = 12;
const EDGE_EXIT_GAP: i32 = 36;

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

// ---------- ball mode (dock to the right screen edge → shrink to a ball) ----------
//
// Dragging the card until its right edge touches the monitor's right edge
// collapses it into a small floating ball; dragging the ball away from the
// edge — or simply clicking it — expands the card again at the ball's spot.
// While in ball mode we stop persisting position so the next launch still
// restores the last CARD position.

static BALL_MODE: Mutex<bool> = Mutex::new(false);
/// Brief window after a programmatic resize during which Moved events must
/// not retrigger edge detection (the expand lands flush on the edge).
static BALL_SUPPRESS_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
/// Generation counter for the move debounce: only the latest Moved event's
/// scheduled edge check is allowed to run.
static MOVE_GEN: AtomicU64 = AtomicU64::new(0);

fn ball_suppressed() -> bool {
    BALL_SUPPRESS_UNTIL
        .lock()
        .unwrap()
        .is_some_and(|t| Instant::now() < t)
}

fn in_ball_mode() -> bool {
    *BALL_MODE.lock().unwrap()
}

/// Physical-px right edge of the monitor currently holding the window.
fn monitor_right(window: &tauri::WebviewWindow) -> Option<i32> {
    let m = window.current_monitor().ok().flatten()?;
    Some(m.position().x + m.size().width as i32)
}

fn enter_ball(window: &tauri::WebviewWindow) {
    let Ok(pos) = window.outer_position() else { return };
    *BALL_MODE.lock().unwrap() = true;
    *BALL_SUPPRESS_UNTIL.lock().unwrap() = Some(Instant::now() + Duration::from_millis(800));
    let scale = window.scale_factor().unwrap_or(1.0);
    let ball = (BALL_SIZE * scale).round() as u32;
    let _ = window.set_size(tauri::PhysicalSize::new(ball, ball));
    if let Some(right) = monitor_right(window) {
        let _ = window.set_position(PhysicalPosition::new(right - ball as i32, pos.y));
    }
    let _ = window.emit("ball-mode", true);
}

fn exit_ball(window: &tauri::WebviewWindow) {
    *BALL_MODE.lock().unwrap() = false;
    *BALL_SUPPRESS_UNTIL.lock().unwrap() = Some(Instant::now() + Duration::from_millis(1500));
    let scale = window.scale_factor().unwrap_or(1.0);
    let card_w = (CARD_WIDTH as f64 * scale).round() as u32;
    let card_h = (CARD_HEIGHT as f64 * scale).round() as u32;
    // Expand leftward from the ball so its right edge stays put; clamp to
    // the monitor in case the ball sits near the left edge of another one.
    if let Ok(pos) = window.outer_position() {
        if let Ok(size) = window.outer_size() {
            let mut x = pos.x + size.width as i32 - card_w as i32;
            if let Some(m) = window.current_monitor().ok().flatten() {
                x = x.max(m.position().x);
            }
            let _ = window.set_size(tauri::PhysicalSize::new(card_w, card_h));
            let _ = window.set_position(PhysicalPosition::new(x, pos.y));
            let _ = window.emit("ball-mode", false);
            return;
        }
    }
    let _ = window.set_size(tauri::PhysicalSize::new(card_w, card_h));
    let _ = window.emit("ball-mode", false);
}

/// Ball click → expand. Dragging the ball away from the edge also expands,
/// via the Moved handler below.
#[tauri::command]
fn expand_card(window: tauri::WebviewWindow) {
    if in_ball_mode() {
        exit_ball(&window);
    }
}

// ---------- quota fetch ----------

fn pick<'a>(raw: &'a serde_json::Value, keys: &[&str]) -> Option<&'a serde_json::Value> {
    keys.iter().find_map(|k| raw.get(*k)).filter(|v| !v.is_null())
}

fn ratio_of(stat: Option<&serde_json::Value>, field: &str) -> Option<f64> {
    // proto3 JSON emits doubles as numbers, but tolerate quoted numbers too
    // so a server-side encoding change can never silently read as 0%.
    stat.and_then(|s| s.get(field)).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
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
        Err(e) => json!({ "ok": false, "error": clean_error(&e.to_string()) }),
    }
}

/// Strip internal routing prefixes before an error reaches the card.
fn clean_error(msg: &str) -> String {
    msg.trim_start_matches("AUTH_DEAD: ")
        .trim_start_matches("AUTH_EXPIRED: ")
        .to_string()
}

fn ok_or_err(r: anyhow::Result<serde_json::Value>) -> serde_json::Value {
    match r {
        Ok(v) => v,
        Err(e) => json!({ "ok": false, "error": clean_error(&e.to_string()) }),
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

#[tauri::command]
fn session_status() -> serde_json::Value {
    auth::session_status()
}

/// Grow/shrink the card when the login panel opens/closes. Opening the
/// panel from ball mode first expands the ball back into the card.
#[tauri::command]
fn set_card_height(height: u32, window: tauri::WebviewWindow) {
    if in_ball_mode() {
        exit_ball(&window);
    }
    let _ = window.set_size(tauri::LogicalSize::new(
        CARD_WIDTH as u32,
        height.max(CARD_HEIGHT as u32),
    ));
}

/// Hide the card (same effect as the tray toggle's hide path).
#[tauri::command]
fn hide_card(window: tauri::Window) {
    if let Ok(pos) = window.outer_position() {
        save_position(pos.x, pos.y);
    }
    let _ = window.hide();
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
    let Some(dir) = dirs::config_dir() else { return };
    let p = dir.join("kimi-quota-bar").join("web-login-diag.log");
    use std::io::Write;
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
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
            if let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) {
                let _ = w.close();
            }
            if let Some(m) = app.get_webview_window("main") {
                let _ = m.eval(
                    "window.__kqbPoll && window.__kqbPoll(); window.__kqbSession && window.__kqbSession()",
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

/// One-shot blank/hang watchdog (re-armed on every retry).
fn start_web_login_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(15));
        let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) else { return };
        // Only probe while the window is still on kimi.com.
        let on_kimi = w
            .url()
            .map(|u| u.host_str() == Some("www.kimi.com"))
            .unwrap_or(false);
        if !on_kimi {
            return;
        }
        WEBLOGIN_DIAG_RECEIVED.store(false, Ordering::SeqCst);
        let _ = w.eval(WEB_LOGIN_DIAG_JS);
        std::thread::sleep(Duration::from_secs(3));
        if WEBLOGIN_DIAG_RECEIVED.load(Ordering::SeqCst) {
            return; // probe answered; the nav handler dealt with any blank page
        }
        // Renderer hung — destroy and recreate on the local error page.
        append_diag_log("web-login diag probe unanswered; recreating window");
        let app2 = app.clone();
        let dispatcher = app.clone();
        let _ = dispatcher.run_on_main_thread(move || {
            if let Some(w) = app2.get_webview_window(WEB_LOGIN_LABEL) {
                let _ = w.close();
            }
            build_web_login_window(
                &app2,
                tauri::WebviewUrl::App("weblogin-error.html".into()),
            );
        });
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
    });
}

/// Local error page's retry button: reload kimi.com and re-arm poll+watchdog.
#[tauri::command]
fn retry_web_login(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(WEB_LOGIN_LABEL) {
        let _ = w.eval(&format!("location.replace('{WEB_LOGIN_HOME}')"));
    }
    start_web_login_poll(app.clone());
    start_web_login_watchdog(app);
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

#[tauri::command]
async fn login_qr_poll(code: String) -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(move || {
        ok_or_err(auth::login_qr_poll(&code).map(|mut v| {
            v["ok"] = json!(true);
            v
        }))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))
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
) -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(move || {
        ok_or_err(
            auth::login_sms_verify(&country_code, &number, &verify_code)
                .map(|_| json!({ "ok": true })),
        )
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))
}

#[tauri::command]
async fn logout() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(|| ok_or_err(auth::logout_any()))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }))
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
    let self_check = args
        .iter()
        .any(|a| a == "--check" || a == "--check-refresh" || a == "--check-login" || a == "--check-session" || a == "--check-qr" || a == "--check-profile");
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
        let out = match auth::get_valid_access_token(force)
            .and_then(|t| auth::fetch_quota(&t))
        {
            Ok(raw) => normalize(&raw),
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
        .setup(move |app| {
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

            // Restore last position, else default top-right of the primary
            // monitor. The saved spot is accepted when it sits on ANY
            // connected monitor — multi-display users keep their placement.
            let monitors = window.available_monitors().unwrap_or_default();
            let on_screen = |x: i32, y: i32| {
                monitors.iter().any(|m| {
                    let size = m.size();
                    let origin = m.position();
                    x >= origin.x - 50
                        && y >= origin.y - 50
                        && x + CARD_WIDTH <= origin.x + size.width as i32 + 50
                        && y + 100 <= origin.y + size.height as i32 + 50
                })
            };
            let primary = window.primary_monitor().ok().flatten();
            let fallback = primary.as_ref().map(|m| {
                let size = m.size();
                let origin = m.position();
                (origin.x + size.width as i32 - CARD_WIDTH - 16, origin.y + 32)
            });
            let pos = load_saved_position()
                .filter(|(x, y)| monitors.is_empty() || on_screen(*x, *y))
                .or(fallback);
            if let Some((x, y)) = pos {
                let _ = window.set_position(PhysicalPosition::new(x, y));
            }

            // Save position after dragging and drive ball mode, both on a
            // real debounce: Moved events stop the moment the drag ends, so
            // the edge check is scheduled 400 ms out and only the latest
            // schedule is allowed to run.
            let app_handle = app.handle().clone();
            window.on_window_event(move |event| match event {
                WindowEvent::Moved(_) => {
                    let gen = MOVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
                    let app2 = app_handle.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(400));
                        if MOVE_GEN.load(Ordering::SeqCst) != gen {
                            return;
                        }
                        let dispatcher = app2.clone();
                        let _ = dispatcher.run_on_main_thread(move || {
                            let Some(w) = app2.get_webview_window("main") else { return };
                            let Ok(pos) = w.outer_position() else { return };
                            let Ok(size) = w.outer_size() else { return };
                            if !in_ball_mode() {
                                save_position(pos.x, pos.y);
                                if ball_suppressed() {
                                    return;
                                }
                                let Some(right) = monitor_right(&w) else { return };
                                let gap = right - (pos.x + size.width as i32);
                                if (0..=EDGE_ENTER_GAP).contains(&gap) {
                                    enter_ball(&w);
                                }
                            } else {
                                let Some(right) = monitor_right(&w) else { return };
                                let gap = right - (pos.x + size.width as i32);
                                if gap.abs() > EDGE_EXIT_GAP {
                                    exit_ball(&w);
                                }
                            }
                        });
                    });
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

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_quota,
            start_drag,
            session_status,
            set_card_height,
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
            get_profile,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kimi Quota Bar");
}
