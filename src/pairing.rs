use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::adb::Adb;

#[derive(Clone)]
pub struct PairingQr {
    /// 二维码载荷（AOSP 格式）
    pub payload: String,
    /// mDNS 服务实例名（S 字段）
    pub service_instance: String,
    /// 配对密码（P 字段）
    pub password: String,
}

/// 生成一次配对会话：6 位数字密码
///
/// 载荷格式（与 Android / escrcpy 兼容）：
/// `WIFI:T:ADB;S:ADBQR-connectPhoneOverWifi;P:<password>;;`
/// 注意 S 字段固定为 `ADBQR-connectPhoneOverWifi`：手机扫码后广播的是
/// **手机自己的** `_adb-tls-pairing._tcp` mDNS 服务（实例名与 S 无关），
/// PC 端扫描到任意 pairing 服务后用 P 字段的密码执行 `adb pair`。
pub fn generate() -> PairingQr {
    let service_instance = "ADBQR-connectPhoneOverWifi".to_string();
    let password: String = (0..6)
        .map(|_| (rand::random::<u32>() % 10).to_string())
        .collect();
    let payload = format!("WIFI:T:ADB;S:{service_instance};P:{password};;");
    PairingQr {
        payload,
        service_instance,
        password,
    }
}

/// 生成二维码位图（含 4 模块静区），返回 (宽, 高, RGB 像素)
pub fn qr_pixels(payload: &str, scale: usize) -> Result<(usize, usize, Vec<u8>), String> {
    let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|e| e.to_string())?;
    let w = code.width() as usize;
    let colors = code.to_colors();
    let quiet = 4usize;
    let size = (w + quiet * 2) * scale;
    let mut px = vec![255u8; size * size * 3];
    for i in 0..w {
        for j in 0..w {
            let dark = colors[i * w + j] == qrcode::types::Color::Dark;
            if !dark {
                continue;
            }
            let base_x = (j + quiet) * scale;
            let base_y = (i + quiet) * scale;
            for dy in 0..scale {
                for dx in 0..scale {
                    let x = base_x + dx;
                    let y = base_y + dy;
                    let o = (y * size + x) * 3;
                    px[o] = 0;
                    px[o + 1] = 0;
                    px[o + 2] = 0;
                }
            }
        }
    }
    Ok((size, size, px))
}

/// 后台配对流程（与 escrcpy 一致的机制）：
/// 1. 轮询 `adb mdns services` 等 `_adb-tls-pairing._tcp` 服务出现
///    （手机扫码后广播的是手机自己的实例名，因此**不匹配二维码 S 字段**）
/// 2. 对发现的 pairing 服务逐个执行 `adb pair host:port 密码`（多台手机时可能连错，
///    失败则尝试下一个）
/// 3. 配对成功后等 `_adb-tls-connect._tcp` 出现并 `adb connect`
pub fn pair_loop(
    adb_path: PathBuf,
    qr: &PairingQr,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let adb = Adb::new(adb_path);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        if Instant::now() > deadline {
            return Err(
                "等待配对超时（2 分钟）。请确认手机与电脑在同一网络，并在手机「无线调试」中点开「使用二维码配对设备」扫描。"
                    .into(),
            );
        }
        let pairing: Vec<_> = adb
            .mdns_services()
            .into_iter()
            .filter(|s| s.service == "_adb-tls-pairing._tcp")
            .collect();
        for svc in &pairing {
            let host_port = format!("{}:{}", svc.host, svc.port);
            match adb.pair(&host_port, &qr.password) {
                Ok(_) => return wait_and_connect(&adb, &svc.host, cancel),
                // 配对失败：可能连到了局域网中另一台开启无线调试的手机，尝试下一个
                Err(_) => continue,
            }
        }
        std::thread::sleep(Duration::from_millis(800));
    }
}

/// 配对成功后，等待手机广播 connect 服务并自动连接
fn wait_and_connect(
    adb: &Adb,
    host: &str,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        if Instant::now() > deadline {
            return Ok("配对成功，等待 adb 自动连接（设备列表将自动刷新）…".into());
        }
        for svc in adb.mdns_services() {
            if svc.service == "_adb-tls-connect._tcp" && svc.host == host {
                let hp = format!("{}:{}", svc.host, svc.port);
                let _ = adb.connect(&hp);
                return Ok(format!("配对成功，已发起连接 {hp}"));
            }
        }
        std::thread::sleep(Duration::from_millis(800));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_format() {
        let qr = generate();
        assert!(qr.payload.starts_with("WIFI:T:ADB;S:ADBQR-connectPhoneOverWifi;P:"));
        assert!(qr.payload.ends_with(";;"));
        assert_eq!(qr.password.len(), 6);
        assert!(qr.password.chars().all(|c| c.is_ascii_digit()));
        assert!(qr.payload.contains(&format!("P:{}", qr.password)));
        assert_eq!(qr.service_instance, "ADBQR-connectPhoneOverWifi");
    }

    #[test]
    fn qr_renders() {
        let (w, h, px) = qr_pixels("WIFI:T:ADB;S:test;P:123456;;", 4).unwrap();
        assert_eq!(w, h);
        assert_eq!(px.len(), w * h * 3);
    }
}
