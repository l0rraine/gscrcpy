use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent};

use crate::adb::{Adb, MdnsService};
use crate::mdns::{resolved_to_service, MdnsCache, CONNECT_TYPE, PAIRING_TYPE};

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

/// 从 receiver 收集一批 mDNS 事件（与 PairProbe 的 services 配合，
/// 先只读收集再更新状态，避免借用冲突）
fn collect_events(rx: &Receiver<ServiceEvent>, out: &mut Vec<ServiceEvent>) {
    let mut handled = 0usize;
    loop {
        if handled >= 50 {
            break;
        }
        let event = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(e) => e,
            Err(_) => break, // 超时/断线都当本轮无更多事件
        };
        handled += 1;
        out.push(event);
    }
}

/// 配对专用 mDNS 探测（对齐 escrcpy：每次配对全新 Bonjour 监听，
/// 避免常驻缓存在网络切换/无线调试重开后收不到手机广播；配对结束即销毁）。
/// mdns-sd 设置了 SO_REUSEPORT，可与常驻缓存共存。
struct PairProbe {
    daemon: Option<ServiceDaemon>,
    rx_pair: Option<Receiver<ServiceEvent>>,
    rx_conn: Option<Receiver<ServiceEvent>>,
    services: HashMap<String, MdnsService>,
}

impl PairProbe {
    fn new() -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
        let rx_pair = daemon.browse(&format!("{PAIRING_TYPE}.local.")).ok();
        let rx_conn = daemon.browse(&format!("{CONNECT_TYPE}.local.")).ok();
        if rx_pair.is_none() && rx_conn.is_none() {
            let _ = daemon.shutdown();
            return Err("mDNS 探测启动失败（端口或网络异常）".into());
        }
        Ok(Self {
            daemon: Some(daemon),
            rx_pair,
            rx_conn,
            services: HashMap::new(),
        })
    }

    /// 拉取事件并返回指定类型的服务快照
    fn services(&mut self, service: &str) -> Vec<MdnsService> {
        // 先只读收集两个 receiver 的事件（避免 &self 不可变借与 &mut self 冲突），
        // 再统一更新服务集合
        let mut events = Vec::new();
        if let Some(rx) = &self.rx_pair {
            collect_events(rx, &mut events);
        }
        if let Some(rx) = &self.rx_conn {
            collect_events(rx, &mut events);
        }
        for event in events {
            match event {
                ServiceEvent::ServiceResolved(rs) => {
                    if let Some(svc) = resolved_to_service(&rs) {
                        let key = format!("{}.{}", svc.instance, svc.service);
                        self.services.insert(key, svc);
                    }
                }
                ServiceEvent::ServiceRemoved(ty, fullname) => {
                    let ty_domain = ty.trim_end_matches('.');
                    let instance = fullname
                        .trim_end_matches(&format!("{ty}."))
                        .trim_end_matches('.')
                        .to_string();
                    let key = format!("{instance}.{ty_domain}");
                    self.services.remove(&key);
                }
                _ => {}
            }
        }
        self.services
            .values()
            .filter(|s| s.service == service)
            .cloned()
            .collect()
    }

    /// 配对失败的服务移出探测集合（避免反复重试已失效广播；手机重新广播会自动加回）
    fn remove_service(&mut self, instance: &str, service: &str) {
        self.services.remove(&format!("{instance}.{service}"));
    }
}

impl Drop for PairProbe {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            let _ = d.shutdown();
        }
    }
}

/// 后台配对流程（与 escrcpy 一致的机制）：
/// 1. 每次配对建立**全新的 mDNS 探测**（对齐 escrcpy 的 fresh Bonjour：网络切换/
///    无线调试重开后常驻 socket 可能收不到广播，fresh 监听必然重新绑定）等
///    `_adb-tls-pairing._tcp` 服务出现（手机扫码后广播的是手机自己的实例名，
///    因此**不匹配二维码 S 字段**）
/// 2. 对发现的 pairing 服务逐个执行 `adb pair host:port 密码`（多台手机时可能连错，
///    失败则尝试下一个；失败服务立即移出候选，避免死循环重试已失效广播）
/// 3. 配对成功后等 `_adb-tls-connect._tcp` 出现并 `adb connect`；
///    connect 服务超时未出现时对齐 escrcpy fallback 尝试默认端口 5555
/// 全程通过 log 回调上报真实输出/错误，供 UI「配对过程日志」展示。
pub fn pair_loop(
    adb_path: PathBuf,
    qr: &PairingQr,
    cancel: &AtomicBool,
    mdns: &MdnsCache,
    log: &dyn Fn(&str),
) -> Result<String, String> {
    let adb = Adb::new(adb_path);
    let deadline = Instant::now() + Duration::from_secs(120);
    // 对齐 escrcpy：每次配对建立全新 mDNS 探测；启动失败时退回常驻缓存
    let mut probe = PairProbe::new().ok();
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
        let pairing: Vec<MdnsService> = match probe.as_mut() {
            Some(p) => p.services(PAIRING_TYPE),
            None => mdns.services(PAIRING_TYPE),
        };
        if pairing.is_empty() {
            log("尚未发现配对服务（_adb-tls-pairing），等待手机扫码广播…");
        }
        for svc in &pairing {
            let host_port = format!("{}:{}", svc.host, svc.port);
            log(&format!(
                "发现配对服务 {}（{}），尝试 adb pair {host_port}…",
                svc.instance, svc.service
            ));
            match adb.pair(&host_port, &qr.password) {
                Ok(o) => {
                    log(&format!("配对 {host_port} 成功: {o}"));
                    return wait_and_connect(&adb, &svc.host, cancel, &mut probe, mdns, log);
                }
                // 配对失败：可能连到了局域网中另一台开启无线调试的手机，或该广播已失效
                // （取消配对/页面关闭）；失败服务移出候选，手机重新广播时会再次出现
                Err(e) => {
                    log(&format!("配对 {host_port} 失败: {e}，移出候选继续等待新广播"));
                    if let Some(p) = probe.as_mut() {
                        p.remove_service(&svc.instance, &svc.service);
                    } else {
                        mdns.remove_service(&svc.instance, &svc.service);
                    }
                    continue;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(800));
    }
}

/// 配对码手动配对（二维码失败的可靠兜底）：
/// 直接对用户从手机「无线调试 → 使用配对码配对设备」页面看到的 ip:port 执行 adb pair，
/// 成功后等待该主机广播 connect 服务并自动连接。
pub fn pair_manual(
    adb_path: PathBuf,
    host_port: &str,
    code: &str,
    cancel: &AtomicBool,
    mdns: &MdnsCache,
    log: &dyn Fn(&str),
) -> Result<String, String> {
    let adb = Adb::new(adb_path);
    let mut probe = PairProbe::new().ok();
    log(&format!("执行配对: adb pair {host_port} {code}"));
    match adb.pair(host_port, code) {
        Ok(o) => {
            log(&format!("配对 {host_port} 成功: {o}"));
            let host = host_port
                .rsplit_once(':')
                .map(|(h, _)| h.to_string())
                .unwrap_or_default();
            wait_and_connect(&adb, &host, cancel, &mut probe, mdns, log)
        }
        Err(e) => Err(format!("配对失败: {e}")),
    }
}

/// 配对成功后，等待手机广播 connect 服务并自动连接；
/// connect 服务超时未出现时，对齐 escrcpy fallback 尝试默认端口 5555
fn wait_and_connect(
    adb: &Adb,
    host: &str,
    cancel: &AtomicBool,
    probe: &mut Option<PairProbe>,
    mdns: &MdnsCache,
    log: &dyn Fn(&str),
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("已取消".into());
        }
        let conn: Vec<MdnsService> = match probe.as_mut() {
            Some(p) => p.services(CONNECT_TYPE),
            None => mdns.services(CONNECT_TYPE),
        };
        for svc in &conn {
            if svc.host == host {
                let hp = format!("{}:{}", svc.host, svc.port);
                let _ = adb.connect(&hp);
                log(&format!("发现 connect 服务，已发起连接 {hp}"));
                return Ok(format!("配对成功，已发起连接 {hp}"));
            }
        }
        if Instant::now() > deadline {
            // 对齐 escrcpy fallback：connect 服务未发现时试无线调试默认端口 5555
            let hp = format!("{host}:5555");
            match adb.connect(&hp) {
                Ok(o) => {
                    return Ok(format!("配对成功，已通过默认端口连接 {hp} ({o})"));
                }
                Err(e) => {
                    log(&format!("默认端口 {hp} 连接失败: {e}，等待设备列表自动刷新…"));
                }
            }
            return Ok("配对成功，等待 adb 自动连接（设备列表将自动刷新）…".into());
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
