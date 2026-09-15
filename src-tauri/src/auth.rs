//! Reads the kimi-desktop local token store and calls the official quota API.
//!
//! The Kimi desktop app keeps its session tokens in
//! `bridge-store/token-store.json`, encrypted with Electron safeStorage
//! (macOS: AES-128-CBC, key derived from a Keychain item via PBKDF2;
//! Windows: DPAPI). We decrypt it locally — nothing leaves the machine
//! except the quota API call itself.

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;

const TOKEN_STORE_RELATIVE: &str = "kimi-desktop/bridge-store/token-store.json";
const QUOTA_URL: &str =
    "https://www.kimi.com/apiv2/kimi.gateway.membership.v2.MembershipService/GetSubscriptionStats";

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

/// Load the current access token from the local kimi-desktop token store.
pub fn load_access_token() -> Result<String> {
    let path = token_store_path()?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("无法读取令牌文件: {}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    let encryption = v["encryption"].as_str().unwrap_or_default();
    if !encryption.starts_with("safeStorage") {
        return Err(anyhow!("未知的令牌加密方式: {encryption}"));
    }
    let data_b64 = v["data"]
        .as_str()
        .ok_or_else(|| anyhow!("令牌文件缺少 data 字段"))?;
    let blob = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
    let plaintext = decrypt_safe_storage(&blob)?;
    let doc: serde_json::Value = serde_json::from_str(&plaintext)?;
    doc["tokens"]["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("令牌文件中缺少 access_token"))
}

#[cfg(target_os = "macos")]
fn decrypt_safe_storage(blob: &[u8]) -> Result<String> {
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

    if !blob.starts_with(b"v10") {
        return Err(anyhow!("不支持的 safeStorage 数据前缀"));
    }
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
    let iv = [0x20u8; 16]; // 16 spaces, per Chromium OSCrypt on macOS

    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
    let mut buf = blob[3..].to_vec();
    let pt = Aes128CbcDec::new(&key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| anyhow!("AES 解密失败: {e}"))?;
    Ok(String::from_utf8(pt.to_vec())?)
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

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn decrypt_safe_storage(_blob: &[u8]) -> Result<String> {
    Err(anyhow!("unsupported platform"))
}

/// Call the official quota endpoint. Returns the raw JSON payload.
pub fn fetch_quota(access_token: &str) -> Result<serde_json::Value> {
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
                Err(anyhow!("登录已过期，请在 Kimi 客户端重新登录"))
            } else {
                Err(anyhow!("额度接口返回 {code}"))
            }
        }
        Err(e) => Err(anyhow!("额度接口请求失败: {e}")),
    }
}
