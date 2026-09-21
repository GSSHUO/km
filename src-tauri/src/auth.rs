//! Reads the kimi-desktop local token store and calls the official quota API,
//! with automatic token renewal.
//!
//! The Kimi desktop app keeps its session tokens in
//! `bridge-store/token-store.json`, encrypted with Chromium OSCrypt
//! (macOS: AES-128-CBC, key derived from a Keychain item via PBKDF2;
//! Windows: AES-256-GCM, key DPAPI-wrapped in `Local State` — legacy
//! builds used raw DPAPI). We decrypt it locally — nothing leaves the
//! machine except the official API calls themselves.
//!
//! Token lifetimes: the access_token minted by the desktop client's own
//! refresh loop lives ~15 minutes, so a closed client means a dead token.
//! The refresh_token lives ~90 days. This module renews the pair itself via
//! the official endpoint `GET /api/auth/token/refresh` and writes the rotated
//! tokens back into the store, keeping the desktop client usable.

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use serde_json::Value;

const TOKEN_STORE_RELATIVE: &str = "kimi-desktop/bridge-store/token-store.json";
const QUOTA_URL: &str =
    "https://www.kimi.com/apiv2/kimi.gateway.membership.v2.MembershipService/GetSubscriptionStats";
/// Official token refresh endpoint (same one the desktop client uses).
const REFRESH_URL: &str = "https://www.kimi.com/api/auth/token/refresh";
/// Renew when the stored access token dies within this window.
const REFRESH_SKEW_SECS: i64 = 120;

fn token_store_path() -> Result<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let p = dirs::home_dir()
            .ok_or_else(|| anyhow!("no home dir"))?
            .join("Library/Application Support")
            .join(TOKEN_STORE_RELATIVE);
        return Ok(p);
    }
    #[cfg(target_os = "windows")]
    {
        let base = dirs::config_dir().ok_or_else(|| anyhow!("no config dir"))?;
        for app_dir in ["kimi-desktop", "Kimi"] {
            let p = base.join(app_dir).join("bridge-store/token-store.json");
            if p.exists() {
                return Ok(p);
            }
        }
        Ok(base.join(TOKEN_STORE_RELATIVE))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(anyhow!("unsupported platform"))
    }
}

// ---------- safeStorage crypto ----------

#[cfg(target_os = "macos")]
fn keychain_password(service: &str) -> Result<String> {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .context("无法调用 security 读取钥匙串")?;
    if !out.status.success() {
        return Err(anyhow!("钥匙串读取被拒绝，请允许访问 \"{service}\""));
    }
    Ok(String::from_utf8(out.stdout)?.trim_end().to_string())
}

#[cfg(target_os = "macos")]
fn oscrypt_key_from(password: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), b"saltysalt", 1003, &mut key);
    key
}

#[cfg(target_os = "macos")]
fn safe_storage_key() -> Result<[u8; 16]> {
    // Electron's Safe Storage password lives in the login Keychain.
    // First run triggers a one-time macOS consent prompt for our binary.
    Ok(oscrypt_key_from(&keychain_password("kimi-desktop Safe Storage")?))
}

/// OSCrypt AES-128-CBC ("v10" || ciphertext, IV = 16 spaces), parameterized
/// by key so the desktop store and our own session file can share it.
#[cfg(target_os = "macos")]
fn oscrypt_decrypt(key: &[u8; 16], blob: &[u8]) -> Result<String> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

    if !blob.starts_with(b"v10") {
        return Err(anyhow!("不支持的加密数据前缀"));
    }
    let iv = [0x20u8; 16]; // 16 spaces, per Chromium OSCrypt on macOS
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
    let cipher = Aes128CbcDec::new_from_slice(key).map_err(|e| anyhow!("AES 密钥错误: {e}"))?;
    let mut buf = blob[3..].to_vec();
    let pt = cipher
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| anyhow!("AES 解密失败: {e}"))?;
    Ok(String::from_utf8(pt.to_vec())?)
}

#[cfg(target_os = "macos")]
fn oscrypt_encrypt(key: &[u8; 16], plaintext: &str) -> Result<Vec<u8>> {
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    let iv = [0x20u8; 16];
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
    let cipher = Aes128CbcEnc::new_from_slice(key).map_err(|e| anyhow!("AES 密钥错误: {e}"))?;
    let msg = plaintext.as_bytes();
    // cipher is built without `alloc` here, so pad the buffer manually and
    // use encrypt_padded_mut (block-aligned length, Pkcs7 fills the rest).
    let mut buf = msg.to_vec();
    buf.resize(msg.len() + (16 - msg.len() % 16), 0);
    let ct = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buf, msg.len())
        .map_err(|e| anyhow!("AES 加密失败: {e}"))?
        .to_vec();
    let mut out = b"v10".to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

#[cfg(target_os = "macos")]
fn decrypt_safe_storage(blob: &[u8]) -> Result<String> {
    oscrypt_decrypt(&safe_storage_key()?, blob)
}

#[cfg(target_os = "macos")]
fn encrypt_safe_storage(plaintext: &str) -> Result<Vec<u8>> {
    oscrypt_encrypt(&safe_storage_key()?, plaintext)
}

// Windows: current kimi-desktop builds wrap tokens with Chromium OSCrypt —
// AES-256-GCM whose key lives in `Local State` under
// `os_crypt.encrypted_key`, itself DPAPI-wrapped. Older builds used raw
// DPAPI (Electron safeStorage). OSCrypt is tried first, DPAPI is the
// fallback for both directions.

#[cfg(target_os = "windows")]
fn dpapi_unprotect(cipher: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: cipher.len() as u32,
        pbData: cipher.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(anyhow!("DPAPI 解密失败"));
    }
    let data = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(output.pbData as _);
    }
    Ok(data)
}

#[cfg(target_os = "windows")]
fn dpapi_protect(plaintext: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len() as u32,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(anyhow!("DPAPI 加密失败"));
    }
    let data = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(output.pbData as _);
    }
    Ok(data)
}

/// Unwrap the OSCrypt AES-256 key from `Local State` (sits next to
/// `bridge-store/`, so it follows whichever app dir was resolved).
#[cfg(target_os = "windows")]
fn os_crypt_key() -> Result<[u8; 32]> {
    let store = token_store_path()?;
    let app_dir = store
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow!("无法定位应用数据目录"))?;
    let raw = std::fs::read_to_string(app_dir.join("Local State"))
        .context("无法读取 Local State（kimi-desktop 加密密钥库）")?;
    let v: Value = serde_json::from_str(&raw)?;
    let enc = v["os_crypt"]["encrypted_key"]
        .as_str()
        .ok_or_else(|| anyhow!("Local State 缺少 os_crypt.encrypted_key"))?;
    let blob = base64::engine::general_purpose::STANDARD.decode(enc)?;
    if !blob.starts_with(b"DPAPI") {
        return Err(anyhow!("未知的 OSCrypt 密钥包装方式"));
    }
    let key = dpapi_unprotect(&blob[5..])?;
    key.try_into().map_err(|_| anyhow!("OSCrypt 密钥长度异常"))
}

/// Chromium OSCrypt v10 layout: "v10" || 12-byte nonce || ciphertext || 16-byte tag.
#[cfg(target_os = "windows")]
fn aes_gcm_decrypt(blob: &[u8]) -> Result<String> {
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};

    if !blob.starts_with(b"v10") || blob.len() < 3 + 12 + 16 {
        return Err(anyhow!("不是有效的 v10 AES-GCM 数据"));
    }
    let key = os_crypt_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("AES 密钥初始化失败: {e}"))?;
    let pt = cipher
        .decrypt(Nonce::from_slice(&blob[3..15]), &blob[15..])
        .map_err(|_| anyhow!("AES-GCM 解密失败（密钥不匹配）"))?;
    Ok(String::from_utf8(pt)?)
}

#[cfg(target_os = "windows")]
fn aes_gcm_encrypt(plaintext: &str) -> Result<Vec<u8>> {
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
    use rand::RngCore;

    let key = os_crypt_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("AES 密钥初始化失败: {e}"))?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|_| anyhow!("AES-GCM 加密失败"))?;
    let mut out = b"v10".to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

#[cfg(target_os = "windows")]
fn decrypt_safe_storage(blob: &[u8]) -> Result<String> {
    match aes_gcm_decrypt(blob) {
        Ok(s) => Ok(s),
        Err(e) => {
            log::warn!("OSCrypt 解密失败，回退旧版 DPAPI: {e}");
            let cipher = if blob.starts_with(b"v10") { &blob[3..] } else { blob };
            Ok(String::from_utf8(dpapi_unprotect(cipher)?)?)
        }
    }
}

#[cfg(target_os = "windows")]
fn encrypt_safe_storage(plaintext: &str) -> Result<Vec<u8>> {
    match aes_gcm_encrypt(plaintext) {
        Ok(v) => Ok(v),
        Err(e) => {
            log::warn!("OSCrypt 加密失败，回退旧版 DPAPI: {e}");
            let mut out = b"v10".to_vec();
            out.extend_from_slice(&dpapi_protect(plaintext.as_bytes())?);
            Ok(out)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn decrypt_safe_storage(_blob: &[u8]) -> Result<String> {
    Err(anyhow!("unsupported platform"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn encrypt_safe_storage(_plaintext: &str) -> Result<Vec<u8>> {
    Err(anyhow!("unsupported platform"))
}

// ---------- token store I/O ----------

/// Read + decrypt the whole token-store document (fields preserved verbatim).
fn load_store() -> Result<Value> {
    let path = token_store_path()?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("无法读取令牌文件: {}", path.display()))?;
    let v: Value = serde_json::from_str(&raw)?;
    let encryption = v["encryption"].as_str().unwrap_or_default();
    if !encryption.starts_with("safeStorage") {
        return Err(anyhow!("未知的令牌加密方式: {encryption}"));
    }
    let data_b64 = v["data"]
        .as_str()
        .ok_or_else(|| anyhow!("令牌文件缺少 data 字段"))?;
    let blob = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
    let plaintext = decrypt_safe_storage(&blob)?;
    Ok(serde_json::from_str(&plaintext)?)
}

/// Encrypt + atomically write the token-store document back.
fn save_store(doc: &Value) -> Result<()> {
    let path = token_store_path()?;
    let plaintext = serde_json::to_string(doc)?;
    let blob = encrypt_safe_storage(&plaintext)?;
    let out = serde_json::json!({
        "encryption": "safeStorage.v1",
        "data": base64::engine::general_purpose::STANDARD.encode(blob),
    });
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(&out)?)
        .with_context(|| format!("无法写入临时令牌文件: {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("无法替换令牌文件: {}", path.display()))?;
    Ok(())
}

// ---------- token freshness ----------

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Extract `exp` from a JWT payload; None when unparseable.
fn jwt_exp(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    parts.next()?;
    let payload = parts.next()?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    v["exp"].as_i64()
}

/// The user id embedded in a JWT (`sub` claim), for the personal center.
fn jwt_sub(token: &str) -> Option<String> {
    let mut parts = token.split('.');
    parts.next()?;
    let payload = parts.next()?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    v["sub"].as_str().map(|s| s.to_string())
}

/// User id of the account the card currently follows, from its token.
pub fn current_user_id() -> Option<String> {
    jwt_sub(&get_valid_access_token(false).ok()?)
}

/// Call the official refresh endpoint. Returns (access_token, Option<rotated refresh_token>).
fn refresh_tokens(refresh_token: &str) -> Result<(String, Option<String>)> {
    let resp = ureq::get(REFRESH_URL)
        .set("Authorization", &format!("Bearer {refresh_token}"))
        .set("x-msh-platform", "web")
        .timeout(std::time::Duration::from_secs(15))
        .call();
    match resp {
        Ok(r) => {
            let v: Value = r.into_json().context("刷新接口响应解析失败")?;
            let at = v["access_token"]
                .as_str()
                .ok_or_else(|| anyhow!("刷新接口未返回 access_token"))?
                .to_string();
            let rt = v["refresh_token"].as_str().map(|s| s.to_string());
            Ok((at, rt))
        }
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(400, _)) => Err(anyhow!(
            "AUTH_DEAD: 刷新令牌已失效，请重新登录"
        )),
        Err(ureq::Error::Status(code, _)) => Err(anyhow!("刷新接口返回 {code}")),
        Err(e) => Err(anyhow!("刷新接口请求失败: {e}")),
    }
}

/// Serializes token acquisition process-wide: concurrent pollers must not
/// race the refresh endpoint. Whoever loses the race simply re-reads the
/// store the winner just wrote and reuses its fresh token.
static TOKEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Get a usable access token: reuse the stored one while fresh, otherwise
/// renew the pair via the official endpoint and persist the rotation.
/// `force_refresh` skips the freshness check (used after a 401).
pub fn get_valid_access_token(force_refresh: bool) -> Result<String> {
    // Poisoning is tolerated: a panic mid-refresh leaves no shared state
    // behind, so a poisoned lock must not block future refreshes.
    let _guard = TOKEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 1) Our own session (in-app QR/SMS login) wins when present.
    if let Some(sess) = load_session().unwrap_or(None) {
        let at = sess["access_token"].as_str().unwrap_or_default();
        let fresh = match jwt_exp(at) {
            Some(exp) => exp > now_secs() + REFRESH_SKEW_SECS,
            None => true,
        };
        if !force_refresh && fresh && !at.is_empty() {
            return Ok(at.to_string());
        }
        let rt = sess["refresh_token"].as_str().unwrap_or_default();
        match refresh_tokens(rt) {
            Ok((new_at, new_rt)) => {
                let mut s = sess.clone();
                s["access_token"] = Value::String(new_at.clone());
                if let Some(r) = new_rt {
                    s["refresh_token"] = Value::String(r);
                }
                if let Err(e) = save_session(&s) {
                    log::warn!("自有会话写回失败（下次会重新刷新）: {e}");
                }
                return Ok(new_at);
            }
            Err(e) if e.to_string().starts_with("AUTH_DEAD") => {
                // Server rejected the refresh token: drop the dead session
                // and fall through to the desktop client's session.
                log::warn!("自有会话已失效，清除并回落到客户端会话");
                let _ = delete_session();
            }
            Err(e) => return Err(e),
        }
    }

    // 2) Desktop client's token store.
    // Explicit sign-out marker: the card stops following the desktop
    // client's session until the next in-card login clears it.
    if is_logged_out() {
        return Err(anyhow!("已退出登录，请重新登录"));
    }

    let mut doc = load_store()?;
    let stored_at = doc["tokens"]["access_token"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let fresh = match jwt_exp(&stored_at) {
        Some(exp) => exp > now_secs() + REFRESH_SKEW_SECS,
        // Unparseable token: try using it; the quota call's 401 path retries.
        None => true,
    };
    if !force_refresh && fresh && !stored_at.is_empty() {
        return Ok(stored_at);
    }

    let rt = doc["tokens"]["refresh_token"]
        .as_str()
        .ok_or_else(|| anyhow!("令牌文件中缺少 refresh_token，请打开 Kimi 客户端登录"))?;
    let (new_at, new_rt) = refresh_tokens(rt)?;

    doc["tokens"]["access_token"] = Value::String(new_at.clone());
    if let Some(rt2) = new_rt {
        doc["tokens"]["refresh_token"] = Value::String(rt2);
    }
    // Persist the rotated pair so the desktop client keeps working.
    // The refresh endpoint tolerates reuse within a grace window, so a
    // failed write is not fatal: the in-memory token is still returned.
    if let Err(e) = save_store(&doc) {
        log::warn!("令牌写回失败（下次会重新刷新）: {e}");
    }
    Ok(new_at)
}

// ---------- quota API ----------

/// Call the official quota endpoint. Returns the raw JSON payload.
/// Auth failures are prefixed with `AUTH_EXPIRED:` for the retry logic.
pub fn fetch_quota(access_token: &str) -> Result<Value> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .build();
    let resp = agent
        .post(QUOTA_URL)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("x-msh-platform", "web")
        .send_string("{}");
    match resp {
        Ok(r) => Ok(r.into_json().context("额度接口响应解析失败")?),
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            if code == 401 || body.contains("unauthenticated") {
                Err(anyhow!("AUTH_EXPIRED: 登录已过期"))
            } else {
                Err(anyhow!("额度接口返回 {code}"))
            }
        }
        Err(e) => Err(anyhow!("额度接口请求失败: {e}")),
    }
}

/// Fetch quota, transparently renewing the session when needed:
/// fresh stored token → forced refresh on 401 (one retry).
pub fn fetch_quota_smart() -> Result<Value> {
    let token = get_valid_access_token(false)?;
    match fetch_quota(&token) {
        Err(e) if e.to_string().starts_with("AUTH_EXPIRED") => {
            let token = get_valid_access_token(true)?;
            fetch_quota(&token)
        }
        other => other,
    }
}


// ---------- own session store (in-app login) ----------
//
// The app may keep its own login session, encrypted at rest and separate
// from the desktop client's token store. Windows: DPAPI blob. macOS:
// AES-128-CBC with a key derived from our own Keychain item (created on
// first login). Our session wins over the desktop session when present;
// deleting it falls back to following the desktop client.

fn session_path() -> Result<std::path::PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("no config dir"))?
        .join("kimi-quota-bar");
    let _ = std::fs::create_dir_all(&dir);
    Ok(dir.join("session.json"))
}

#[cfg(target_os = "windows")]
fn session_encrypt(plaintext: &str) -> Result<Vec<u8>> {
    dpapi_protect(plaintext.as_bytes())
}

#[cfg(target_os = "windows")]
fn session_decrypt(blob: &[u8]) -> Result<String> {
    Ok(String::from_utf8(dpapi_unprotect(blob)?)?)
}

#[cfg(target_os = "macos")]
fn session_key() -> Result<[u8; 16]> {
    const SERVICE: &str = "kimi-quota-bar Session Key";
    match keychain_password(SERVICE) {
        Ok(pw) => Ok(oscrypt_key_from(&pw)),
        Err(_) => {
            // First login on this machine: mint a random keychain item.
            let mut bytes = [0u8; 16];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
            let pw: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let out = std::process::Command::new("security")
                .args([
                    "add-generic-password",
                    "-s",
                    SERVICE,
                    "-a",
                    "kimi-quota-bar",
                    "-w",
                    &pw,
                    "-U",
                ])
                .output()
                .context("无法调用 security 写入钥匙串")?;
            if !out.status.success() {
                return Err(anyhow!("无法创建会话钥匙串项"));
            }
            Ok(oscrypt_key_from(&pw))
        }
    }
}

#[cfg(target_os = "macos")]
fn session_encrypt(plaintext: &str) -> Result<Vec<u8>> {
    oscrypt_encrypt(&session_key()?, plaintext)
}

#[cfg(target_os = "macos")]
fn session_decrypt(blob: &[u8]) -> Result<String> {
    oscrypt_decrypt(&session_key()?, blob)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn session_encrypt(_plaintext: &str) -> Result<Vec<u8>> {
    Err(anyhow!("unsupported platform"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn session_decrypt(_blob: &[u8]) -> Result<String> {
    Err(anyhow!("unsupported platform"))
}

pub fn load_session() -> Result<Option<Value>> {
    let path = session_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).context("无法读取会话文件")?;
    let v: Value = serde_json::from_str(&raw)?;
    let data_b64 = v["data"]
        .as_str()
        .ok_or_else(|| anyhow!("会话文件缺少 data 字段"))?;
    let blob = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
    let plaintext = session_decrypt(&blob)?;
    Ok(Some(serde_json::from_str(&plaintext)?))
}

pub fn save_session(doc: &Value) -> Result<()> {
    let path = session_path()?;
    let blob = session_encrypt(&serde_json::to_string(doc)?)?;
    let out = serde_json::json!({
        "encryption": "kqb.v1",
        "data": base64::engine::general_purpose::STANDARD.encode(blob),
    });
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(&out)?)?;
    std::fs::rename(&tmp, &path)?;
    // A successful in-card login always re-enables desktop-session following.
    set_logged_out(false);
    Ok(())
}

// ---------- explicit sign-out marker ----------
//
// The official Logout API does NOT actually invalidate these stateless JWTs
// (verified: quota + refresh keep working after it returns 200), so signing
// out is a local affair. The marker tells the card to stop following the
// desktop client's session; the next in-card login clears it.

fn logged_out_marker() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("kimi-quota-bar").join("logged-out"))
}

pub fn is_logged_out() -> bool {
    logged_out_marker().map(|p| p.exists()).unwrap_or(false)
}

fn set_logged_out(out: bool) {
    if let Some(p) = logged_out_marker() {
        if out {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, b"");
        } else if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub fn delete_session() -> Result<()> {
    let path = session_path()?;
    if path.exists() {
        std::fs::remove_file(&path).context("无法删除会话文件")?;
    }
    Ok(())
}

/// What the UI needs to render the account state. `followingDesktop` just
/// reports that a desktop token store exists (cheap file check, no network)
/// so the account button can offer logout for that session too.
pub fn session_status() -> Value {
    match load_session() {
        Ok(Some(s)) => serde_json::json!({
            "ownSession": true,
            "userId": s["user_id"].as_str(),
            "source": s["source"].as_str(),
        }),
        _ => serde_json::json!({
            "ownSession": false,
            "followingDesktop": token_store_path().map(|p| p.exists()).unwrap_or(false),
            "loggedOut": is_logged_out(),
        }),
    }
}

// ---------- official auth API (login / logout) ----------
//
// Connect-RPC JSON endpoints under https://auth.kimi.com/api, matching the
// official web client (proto field names on the wire). Verified working:
// CreateLoginQRCode / GetLoginQRCodeStatus. SMS sending additionally
// requires a server-side captcha token; that error is surfaced verbatim so
// the UI can steer the user to QR login.

const AUTH_API: &str = "https://auth.kimi.com/api";
const AUTH_SVC: &str = "account.gateway.v1.AuthService";
const SMS_SVC: &str = "account.gateway.v1.SMSService";

/// Stable per-install device id in the Volcano-Engine webId format (19-digit
/// numeric string). The mobile confirm step expects it in the scanned URL —
/// the official web client refuses to even render the QR before its webId
/// arrives, so a URL without device_id leaves the ticket PENDING forever.
pub fn device_id() -> String {
    let path = dirs::config_dir().map(|d| d.join("kimi-quota-bar").join("device_id"));
    if let Some(p) = &path {
        if let Ok(s) = std::fs::read_to_string(p) {
            let s = s.trim();
            if s.len() == 19 && s.bytes().all(|b| b.is_ascii_digit()) {
                return s.to_string();
            }
        }
    }
    let mut bytes = [0u8; 19];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    let mut s = String::with_capacity(19);
    s.push((b'1' + bytes[0] % 9) as char);
    for b in &bytes[1..] {
        s.push((b'0' + b % 10) as char);
    }
    if let Some(p) = &path {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, &s);
    }
    s
}

/// Self-check: prove the session crypto round-trips on this machine without
/// touching the real session file.
pub fn session_crypto_roundtrip(probe: &str) -> Result<bool> {
    let blob = session_encrypt(probe)?;
    Ok(session_decrypt(&blob)? == probe)
}

fn auth_rpc(method: &str, body: &str, bearer: Option<&str>) -> Result<Value> {
    let url = format!("{AUTH_API}/{method}");
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .build();
    let mut req = agent
        .post(&url)
        .set("Content-Type", "application/json")
        .set("x-msh-platform", "web");
    if let Some(t) = bearer {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    match req.send_string(body) {
        Ok(r) => r.into_json().context("认证接口响应解析失败"),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            Err(anyhow!("{}", connect_error_message(&text, code)))
        }
        Err(e) => Err(anyhow!("认证接口请求失败: {e}")),
    }
}

/// Extract a readable message from a Connect error envelope.
fn connect_error_message(body: &str, code: u16) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if let Some(m) = v["details"][0]["debug"]["localizedMessage"]["message"].as_str() {
        return m.to_string();
    }
    if let Some(m) = v["details"][0]["debug"]["reason"].as_str() {
        return m.to_string();
    }
    if let Some(m) = v["code"].as_str() {
        return format!("{code} {m}");
    }
    format!("认证接口返回 {code}")
}

/// QR step 1: create a login ticket. The QR image encodes `url`; WeChat or
/// the Kimi mobile app scans it to confirm the login.
pub fn login_qr_create() -> Result<Value> {
    let v = auth_rpc(&format!("{AUTH_SVC}/CreateLoginQRCode"), "{}", None)?;
    let code = v["code"]
        .as_str()
        .ok_or_else(|| anyhow!("未返回二维码 ticket"))?
        .to_string();
    Ok(serde_json::json!({
        "code": code,
        "url": format!("https://www.kimi.com/wechat/mp/auth?id={code}&device_id={}", device_id()),
    }))
}

/// Connect JSON serializes response fields under their proto JSON
/// (camelCase) names; tolerate snake_case too in case the convention flips.
fn field<'a>(v: &'a Value, camel: &str, snake: &str) -> Option<&'a str> {
    v[camel].as_str().or_else(|| v[snake].as_str())
}

/// QR step 2: poll the ticket. On STATUS_SUCCESS the session is persisted
/// here so the next quota fetch picks it up automatically.
pub fn login_qr_poll(code: &str) -> Result<Value> {
    let v = poll_login_qr_status(code)?;
    let status = v["status"].as_str().unwrap_or("STATUS_PENDING").to_string();
    if status == "STATUS_SUCCESS" {
        let at = field(&v, "accessToken", "access_token").unwrap_or_default();
        let rt = field(&v, "refreshToken", "refresh_token").unwrap_or_default();
        if at.is_empty() || rt.is_empty() {
            return Err(anyhow!("登录成功但未返回令牌"));
        }
        save_session(&serde_json::json!({
            "access_token": at,
            "refresh_token": rt,
            "user_id": field(&v, "userId", "user_id").unwrap_or_default(),
            "source": "qr",
        }))?;
    }
    Ok(serde_json::json!({ "status": status }))
}

/// Raw ticket-status query (no persistence) — shared by login_qr_poll and
/// the --check-qr self-test.
pub fn poll_login_qr_status(code: &str) -> Result<Value> {
    let body = serde_json::json!({ "code": code }).to_string();
    auth_rpc(&format!("{AUTH_SVC}/GetLoginQRCodeStatus"), &body, None)
}

/// Confirm a ticket exactly the way the phone app does after a scan.
/// Used by --check-qr to exercise the whole chain without a phone.
pub fn confirm_login_qr(code: &str, bearer: &str) -> Result<()> {
    let body = serde_json::json!({ "code": code, "device_id": device_id() }).to_string();
    auth_rpc(&format!("{AUTH_SVC}/ConfirmLoginQRCode"), &body, Some(bearer))?;
    Ok(())
}

/// Self-test for the QR chain: create + confirm + poll, verify the
/// camelCase token fields parse, persist nothing.
pub fn check_qr_login() -> Result<Value> {
    let ticket = login_qr_create()?;
    let code = ticket["code"].as_str().unwrap_or_default().to_string();
    let token = get_valid_access_token(false)?;
    confirm_login_qr(&code, &token)?;
    let v = poll_login_qr_status(&code)?;
    let at = field(&v, "accessToken", "access_token").unwrap_or_default();
    let rt = field(&v, "refreshToken", "refresh_token").unwrap_or_default();
    Ok(serde_json::json!({
        "ok": v["status"].as_str() == Some("STATUS_SUCCESS") && !at.is_empty() && !rt.is_empty(),
        "status": v["status"],
        "hasAccessToken": !at.is_empty(),
        "hasRefreshToken": !rt.is_empty(),
        "userId": field(&v, "userId", "user_id"),
    }))
}

/// SMS step 1: ask the server to text a code. The server may demand a
/// captcha token we cannot produce — that error reaches the UI verbatim.
pub fn login_sms_send(country_code: &str, number: &str) -> Result<()> {
    let body = serde_json::json!({
        "scene": "SCENE_LOGIN",
        "phone": { "country_code": country_code, "number": number },
    })
    .to_string();
    auth_rpc(&format!("{SMS_SVC}/SendVerifyCode"), &body, None)?;
    Ok(())
}

/// SMS step 2: exchange phone + code for a persisted session.
pub fn login_sms_verify(country_code: &str, number: &str, verify_code: &str) -> Result<()> {
    let body = serde_json::json!({
        "phone": { "country_code": country_code, "number": number },
        "verify_code": verify_code,
    })
    .to_string();
    let v = auth_rpc(&format!("{AUTH_SVC}/LoginWithSMS"), &body, None)?;
    let at = field(&v, "accessToken", "access_token").unwrap_or_default();
    let rt = field(&v, "refreshToken", "refresh_token").unwrap_or_default();
    if at.is_empty() || rt.is_empty() {
        return Err(anyhow!("登录成功但未返回令牌"));
    }
    save_session(&serde_json::json!({
        "access_token": at,
        "refresh_token": rt,
        "user_id": field(&v, "userId", "user_id").unwrap_or_default(),
        "source": "sms",
    }))?;
    Ok(())
}

// ---------- membership summary (personal center) ----------

const MEMBERSHIP_API: &str =
    "https://www.kimi.com/apiv2/kimi.gateway.membership.v2.MembershipService";

/// Plan / level / expiry for the personal-center panel.
pub fn fetch_membership_summary() -> Result<Value> {
    let token = get_valid_access_token(false)?;
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(12))
        .build();
    let v: Value = agent
        .post(&format!("{MEMBERSHIP_API}/GetSubscription"))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {token}"))
        .send_string("{}")
        .map_err(|e| anyhow!("会员信息请求失败: {e}"))?
        .into_json()
        .context("会员信息解析失败")?;
    let sub = &v["subscription"];
    let g = &sub["goods"];
    Ok(serde_json::json!({
        "plan": g["title"].as_str(),
        "level": g["membershipLevel"].as_str(),
        "status": sub["status"].as_str(),
        "active": sub["active"].as_bool().unwrap_or(false),
        "periodEnd": sub["currentEndTime"].as_str(),
        "nextBilling": sub["nextBillingTime"].as_str(),
    }))
}

/// Log out whatever account the card currently follows: revoke our own
/// session server-side (best effort — the API does not truly invalidate
/// JWTs) and delete it, then raise the sign-out marker so the card stops
/// following the desktop client too. The desktop app itself is untouched.
pub fn logout_any() -> Result<Value> {
    if let Some(sess) = load_session()? {
        if let Some(at) = sess["access_token"].as_str() {
            let _ = auth_rpc(&format!("{AUTH_SVC}/Logout"), "{}", Some(at));
        }
        delete_session()?;
    } else if let Ok(at) = get_valid_access_token(false) {
        let _ = auth_rpc(&format!("{AUTH_SVC}/Logout"), "{}", Some(&at));
    }
    set_logged_out(true);
    Ok(session_status())
}
