//! Reads the kimi-desktop local token store and calls the official quota API,
//! with automatic token renewal.
//!
//! The Kimi desktop app keeps its session tokens in
//! `bridge-store/token-store.json`, encrypted with Electron safeStorage
//! (macOS: AES-128-CBC, key derived from a Keychain item via PBKDF2;
//! Windows: DPAPI). We decrypt it locally — nothing leaves the machine
//! except the official API calls themselves.
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
fn safe_storage_key() -> Result<[u8; 16]> {
    // Electron's Safe Storage password lives in the login Keychain.
    // First run triggers a one-time macOS consent prompt for our binary.
    let out = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "kimi-desktop Safe Storage",
            "-w",
        ])
        .output()
        .context("无法调用 security 读取钥匙串")?;
    if !out.status.success() {
        return Err(anyhow!(
            "钥匙串读取被拒绝，请允许访问 \"kimi-desktop Safe Storage\""
        ));
    }
    let password = String::from_utf8(out.stdout)?.trim_end().to_string();
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), b"saltysalt", 1003, &mut key);
    Ok(key)
}

#[cfg(target_os = "macos")]
fn decrypt_safe_storage(blob: &[u8]) -> Result<String> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

    if !blob.starts_with(b"v10") {
        return Err(anyhow!("不支持的 safeStorage 数据前缀"));
    }
    let key = safe_storage_key()?;
    let iv = [0x20u8; 16]; // 16 spaces, per Chromium OSCrypt on macOS
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
    let mut buf = blob[3..].to_vec();
    let pt = Aes128CbcDec::new(&key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| anyhow!("AES 解密失败: {e}"))?;
    Ok(String::from_utf8(pt.to_vec())?)
}

#[cfg(target_os = "macos")]
fn encrypt_safe_storage(plaintext: &str) -> Result<Vec<u8>> {
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};

    let key = safe_storage_key()?;
    let iv = [0x20u8; 16];
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
    let msg = plaintext.as_bytes();
    // cipher is built without `alloc` here, so pad the buffer manually and
    // use encrypt_padded_mut (block-aligned length, Pkcs7 fills the rest).
    let mut buf = msg.to_vec();
    buf.resize(msg.len() + (16 - msg.len() % 16), 0);
    let ct = Aes128CbcEnc::new(&key.into(), &iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, msg.len())
        .map_err(|e| anyhow!("AES 加密失败: {e}"))?
        .to_vec();
    let mut out = b"v10".to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

#[cfg(target_os = "windows")]
fn decrypt_safe_storage(blob: &[u8]) -> Result<String> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let cipher = if blob.starts_with(b"v10") { &blob[3..] } else { blob };
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
    Ok(String::from_utf8(data)?)
}

#[cfg(target_os = "windows")]
fn encrypt_safe_storage(plaintext: &str) -> Result<Vec<u8>> {
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
    let mut out = b"v10".to_vec();
    out.extend_from_slice(&data);
    Ok(out)
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
            "刷新令牌已失效，请打开 Kimi 客户端重新登录一次"
        )),
        Err(ureq::Error::Status(code, _)) => Err(anyhow!("刷新接口返回 {code}")),
        Err(e) => Err(anyhow!("刷新接口请求失败: {e}")),
    }
}

/// Get a usable access token: reuse the stored one while fresh, otherwise
/// renew the pair via the official endpoint and persist the rotation.
/// `force_refresh` skips the freshness check (used after a 401).
pub fn get_valid_access_token(force_refresh: bool) -> Result<String> {
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
