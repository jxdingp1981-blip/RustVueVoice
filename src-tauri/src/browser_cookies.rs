// browser_cookies.rs — 从系统浏览器（Edge/Chrome）读取 doubao.com 域的登录 cookies
// 用途：用户在自己的浏览器里完成豆包登录（手机号+验证码官方流程），
//       本模块收割该域 cookies 组装凭证，替代 WebView 登录窗口。
// 只读取 doubao.com 域的 cookie；解密全部在本机完成，不上传任何数据。
#![allow(dead_code)]

use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;

use crate::creds::AsrParams;

/// 浏览器 cookie 库位置（Local State 与 Cookies 同在 user_data 树下）
struct BrowserDb {
    name: &'static str,
    user_data: PathBuf,
}

fn candidate_browsers() -> Vec<BrowserDb> {
    let local = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
    let roaming = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_default();
    let mut out = vec![
        // 1) Python 版扫码登录留下的 WebView2 持久化会话（零操作直迁）
        BrowserDb {
            name: "WebView2(pywebview 扫码会话)",
            user_data: roaming.join("pywebview").join("EBWebView"),
        },
    ];
    // 2) Python 版 exe 旁的 WebView2 目录（数据目录自定义时）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in ["WebView2/EBWebView", ".WebView2/EBWebView"] {
                out.push(BrowserDb {
                    name: "WebView2(本地会话)",
                    user_data: dir.join(cand),
                });
            }
        }
    }
    // 3) 系统 Edge / Chrome（用户手动登录 doubao.com 后可用）
    out.push(BrowserDb {
        name: "Edge",
        user_data: local.join(r"Microsoft\Edge\User Data"),
    });
    out.push(BrowserDb {
        name: "Chrome",
        user_data: local.join(r"Google\Chrome\User Data"),
    });
    out
}

/// 找到第一个存在 Cookies 库的浏览器 profile（Default / Profile.*）
fn find_cookie_db(browser: &BrowserDb) -> Option<(PathBuf, PathBuf)> {
    for profile in ["Default"] {
        let db = browser.user_data.join(profile).join(r"Network\Cookies");
        if db.is_file() {
            return Some((browser.user_data.clone(), db));
        }
    }
    // 枚举 Profile.* 目录，取最近修改的
    let mut profiles: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&browser.user_data) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("Profile ") || name.starts_with("Profile_") {
                let db = e.path().join(r"Network\Cookies");
                if db.is_file() {
                    let mtime = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                    profiles.push((mtime, e.path()));
                }
            }
        }
    }
    profiles.sort_by(|a, b| b.0.cmp(&a.0));
    profiles.first().map(|(_, p)| {
        (browser.user_data.clone(), p.join(r"Network\Cookies"))
    })
}

/// 读 Local State 里的 AES key（os_crypt.encrypted_key，DPAPI 保护）
fn load_aes_key(user_data: &PathBuf) -> Option<Vec<u8>> {
    let path = user_data.join("Local State");
    let text = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let enc = v.pointer("/os_crypt/encrypted_key")?.as_str()?;
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, enc).ok()?;
    // 去掉 "DPAPI" 前缀（5 字节）后 DPAPI 解密
    if decoded.len() > 5 && &decoded[..5] == b"DPAPI" {
        dpapi_unprotect(&decoded[5..])
    } else {
        dpapi_unprotect(&decoded)
    }
}

/// Windows DPAPI CryptUnprotectData（当前用户上下文）
fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        let ok = CryptUnprotectData(
            &in_blob,
            None,
            None,
            None,
            None,
            0,
            &mut out,
        );
        if ok.is_err() {
            return None;
        }
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let v = slice.to_vec();
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(out.pbData as *mut _)));
        Some(v)
    }
}

/// 解密单个 encrypted_value
/// v10 = AES-256-GCM（key 来自 Local State）；v20 = App-Bound（不支持，报错）；
/// 无前缀 = 旧版 DPAPI 直接加密
fn decrypt_cookie_value(encrypted: &[u8], aes_key: Option<&Vec<u8>>) -> Result<String, String> {
    if encrypted.is_empty() {
        return Ok(String::new());
    }
    if encrypted.len() > 3 && &encrypted[..3] == b"v20" {
        return Err("v20(App-Bound) 加密暂不支持".into());
    }
    if encrypted.len() > 3 && &encrypted[..3] == b"v10" {
        let key = aes_key.ok_or("需要 AES key 但 Local State 读取失败")?;
        if encrypted.len() < 3 + 12 + 16 {
            return Err("v10 数据过短".into());
        }
        let nonce = &encrypted[3..15];
        let cipher = &encrypted[15..];
        let cipher_gcm = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
        let plain = cipher_gcm
            .decrypt(nonce.into(), cipher)
            .map_err(|_| "AES-GCM 解密失败（key 不匹配或数据损坏）".to_string())?;
        // 新版 Chromium（M130+）在解密结果前带 32 字节校验头（HMAC）——剥离后才是真值
        if plain.len() > 32 && plain[..32].iter().any(|&b| b < 0x20 || b > 0x7e) {
            let value = String::from_utf8_lossy(&plain[32..]).into_owned();
            if value.bytes().all(|b| (0x20..0x7f).contains(&b)) {
                return Ok(value);
            }
            crate::applog::warn("[cookie] 剥离 32 字节校验头后仍含非 ASCII 字节，按原样保留");
        }
        return Ok(String::from_utf8_lossy(&plain).into_owned());
    }
    // 旧版：DPAPI
    dpapi_unprotect(encrypted)
        .map(|v| String::from_utf8_lossy(&v).into_owned())
        .ok_or_else(|| "DPAPI 解密失败".into())
}

/// 从浏览器收割 doubao.com cookies。返回 (browser_name, cookies)
pub fn harvest() -> Result<(String, Vec<(String, String)>), String> {
    for b in candidate_browsers() {
        if !b.user_data.is_dir() {
            continue;
        }
        let Some((user_data, db_path)) = find_cookie_db(&b) else { continue };
        // 浏览器运行时 cookie 库被锁：复制到临时文件再开
        let tmp = std::env::temp_dir().join(format!(
            "vm_cookies_{}.db",
            std::process::id()
        ));
        if std::fs::copy(&db_path, &tmp).is_err() {
            continue; // 复制失败 = 库不存在或不可读
        }
        let result = (|| -> Result<Vec<(String, String)>, String> {
            let conn = rusqlite::Connection::open_with_flags(
                &tmp,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| format!("打开 cookie 库失败: {}", e))?;
            let aes_key = load_aes_key(&user_data);
            let mut stmt = conn
                .prepare(
                    "SELECT name, encrypted_value, value FROM cookies \
                     WHERE host_key LIKE '%doubao.com'",
                )
                .map_err(|e| format!("查询失败: {}", e))?;
            let rows = stmt
                .query_map([], |row| {
                    let name: String = row.get(0)?;
                    let enc: Vec<u8> = row.get(1)?;
                    let plain: String = row.get(2)?;
                    Ok((name, enc, plain))
                })
                .map_err(|e| format!("query_map 失败: {}", e))?;
            let mut out = Vec::new();
            for row in rows {
                let (name, enc, plain) = row.map_err(|e| e.to_string())?;
                let value = if !plain.is_empty() {
                    plain
                } else {
                    decrypt_cookie_value(&enc, aes_key.as_ref())
                        .map_err(|e| format!("cookie `{}`: {}", name, e))?
                };
                if !value.is_empty() {
                    out.push((name, value));
                }
            }
            Ok(out)
        })();
        let _ = std::fs::remove_file(&tmp);
        match result {
            Ok(cookies) if !cookies.is_empty() => return Ok((b.name.to_string(), cookies)),
            Ok(_) => continue, // 该浏览器无 doubao cookie，试下一个
            Err(e) => {
                crate::applog::warn(&format!("{} cookie 收割失败: {}", b.name, e));
                continue;
            }
        }
    }
    Err("未找到可用的浏览器 cookie（Edge/Chrome 均未找到 doubao.com 的登录 cookie）".into())
}

/// 收割并组装 AsrParams：cookies + 申请/生成 device_id 与 web_id
pub fn harvest_params() -> Result<AsrParams, String> {
    let (browser, cookies) = harvest()?;
    crate::applog::info(&format!(
        "从 {} 收割到 {} 个 doubao.com cookie",
        browser,
        cookies.len()
    ));
    // SSO 登录 cookie 必须在场
    let sso = ["sid_guard", "sid_tt", "uid_tt", "SESSIONID", "sessionid"];
    let names: Vec<&str> = cookies.iter().map(|(k, _)| k.as_str()).collect();
    if !sso.iter().any(|s| names.contains(s)) {
        return Err(format!(
            "浏览器 {} 中未发现豆包登录态（缺少 sid_guard/sid_tt 等 SSO cookie）——\
             请先在该浏览器里打开 doubao.com 完成登录，再回来重试",
            browser
        ));
    }
    let mut map = std::collections::BTreeMap::new();
    for (k, v) in cookies {
        map.insert(k, v);
    }
    let ttwid = map.get("ttwid").cloned().unwrap_or_default();
    let (device_id, web_id) = obtain_ids(&ttwid);
    Ok(AsrParams { cookies: map, device_id, web_id })
}

/// device_id / web_id：优先向字节 webid 接口申请；失败则生成随机 19 位数字
pub fn obtain_ids(_ttwid: &str) -> (String, String) {
    // 先试官方 webid 接口
    if let Some(id) = fetch_webid() {
        return (id.clone(), id);
    }
    // 兜底：19 位随机数字（字节 id 惯例；ASR 会话只透传）
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0x9E3779B97F4A7C15);
    let mut x = seed ^ 0x9E3779B97F4A7C15;
    let mut gen = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let n = gen() % 900_000_000_000_000_000 + 100_000_000_000_000_000;
    let id = n.to_string();
    (id.clone(), id)
}

/// 字节埋点 webid 申请接口（mcs.zijieapi.com/webid，手写 HTTPS POST）
pub fn fetch_webid() -> Option<String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let host = "mcs.zijieapi.com";
    let body = serde_json::json!({
        "app_id": 497858,
        "url": "https://www.doubao.com/chat",
        "user_agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/126.0.0.0 Safari/537.36",
        "referer": "https://www.doubao.com/",
    })
    .to_string();
    let addrs = (host, 443);
    let tcp = TcpStream::connect(addrs).ok()?;
    let connector = native_tls::TlsConnector::new().ok()?;
    let mut tls = connector.connect(host, tcp).ok()?;
    let req = format!(
        "POST /webid?aid=497858 HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nUser-Agent: Mozilla/5.0\r\n\r\n{}",
        host,
        body.len(),
        body
    );
    tls.write_all(req.as_bytes()).ok()?;
    let mut resp = Vec::new();
    tls.read_to_end(&mut resp).ok()?;
    let text = String::from_utf8_lossy(&resp);
    let json_start = text.find('{')?;
    let v: serde_json::Value = serde_json::from_str(&text[json_start..]).ok()?;
    v.get("web_id").and_then(|x| x.as_str()).map(|s| s.to_string())
}
