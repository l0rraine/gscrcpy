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

/// 生成一次配对会话：随机实例名 + 6 位数字密码
///
/// 载荷格式（Android Studio / AOSP）：WIFI:T:ADB;S:<service>;P:<password>;;
pub fn generate() -> PairingQr {
    let service_instance = format!("gscrcpy-{:08x}", rand::random::<u32>());
    let password: String = (0..6)
        .map(|_| (rand::random::<u32>() % 10).to_string())
        .collect();
    let payload = format!("WIFI:T:ADB;S:{};P:{};;", service_instance, password);
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

/// 后台配对流程：
/// 1. 轮询 `adb mdns services` 等待 `_adb-tls-pairing._tcp` 出现（实例名匹配二维码 S）
/// 2. 执行 `adb pair host:port 密码`
/// 3. 等待 `_adb-tls-connect._tcp` 出现并 `adb connect`
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
        for svc in adb.mdns_services() {
            if svc.service == "_adb-tls-pairing._tcp"
                && (svc.instance == qr.service_instance
                    || svc.instance.contains(&qr.service_instance))
            {
                let host_port = format!("{}:{}", svc.host, svc.port);
                adb.pair(&host_port, &qr.password)?;
                return wait_and_connect(&adb, &svc.host, cancel);
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
        assert!(qr.payload.starts_with("WIFI:T:ADB;S:gscrcpy-"));
        assert!(qr.payload.ends_with(";;"));
        assert_eq!(qr.password.len(), 6);
        assert!(qr.password.chars().all(|c| c.is_ascii_digit()));
        assert!(qr.payload.contains(&format!("P:{}", qr.password)));
    }

    #[test]
    fn qr_renders() {
        let (w, h, px) = qr_pixels("WIFI:T:ADB;S:test;P:123456;;", 4).unwrap();
        assert_eq!(w, h);
        assert_eq!(px.len(), w * h * 3);
    }
}
