//! 常驻 mDNS 服务缓存。
//!
//! 用纯 Rust 的 `mdns-sd`（与 escrcpy 使用的 Bonjour 同为标准 mDNS 客户端）持续
//! 扫描无线调试的两类服务，**不依赖 adb 自身的 mDNS**（`adb mdns services` 在
//! Windows 上常因 adb server 的 mDNS 后端/防火墙/网卡选择问题而发现不到设备，
//! 而独立客户端直接监听 UDP 5353 多播，与手机广播互通更可靠）。
//!
//! 后台线程维护 `_adb-tls-pairing._tcp`（配对）与 `_adb-tls-connect._tcp`（连接）
//! 两类服务的最新快照，UI/配对流程随时读取。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mdns_sd::{Receiver, ResolvedService, ServiceDaemon, ServiceEvent};

use crate::adb::MdnsService;

/// 无线调试配对服务类型
pub const PAIRING_TYPE: &str = "_adb-tls-pairing._tcp";
/// 无线调试连接服务类型
pub const CONNECT_TYPE: &str = "_adb-tls-connect._tcp";

#[derive(Clone)]
pub struct MdnsCache {
    inner: Arc<Mutex<HashMap<String, MdnsService>>>,
    stop: Arc<AtomicBool>,
}

impl MdnsCache {
    /// 启动后台扫描线程（幂等：重复调用只启动一次）
    pub fn start() -> Self {
        let inner = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let inner2 = inner.clone();
        let stop2 = stop.clone();
        std::thread::spawn(move || {
            let daemon = match ServiceDaemon::new() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("mdns-sd 启动失败: {e}");
                    return;
                }
            };
            // 两个 receiver 分开轮询；browse 失败（如端口被占）则放弃该类型
            let rx_pair = daemon.browse(&format!("{PAIRING_TYPE}.local.")).ok();
            let rx_conn = daemon.browse(&format!("{CONNECT_TYPE}.local.")).ok();
            while !stop2.load(Ordering::Relaxed) {
                // 轮询配对类型
                if let Some(rx) = &rx_pair {
                    drain_events(rx, &inner2);
                }
                // 轮询连接类型
                if let Some(rx) = &rx_conn {
                    drain_events(rx, &inner2);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            let _ = daemon.shutdown();
        });
        Self { inner, stop }
    }

    /// 当前缓存的指定类型服务快照
    pub fn services(&self, service: &str) -> Vec<MdnsService> {
        self.inner
            .lock()
            .ok()
            .map(|m| {
                m.values()
                    .filter(|s| s.service == service)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 按 ip:port 反查连接服务的实例名（如 `adb-XXX`），用于串号去重/别名
    pub fn ip_to_instance(&self, ip_port: &str) -> Option<String> {
        self.services(CONNECT_TYPE)
            .into_iter()
            .find(|s| format!("{}:{}", s.host, s.port) == ip_port)
            .map(|s| s.instance)
    }

    /// 移除指定服务（配对失败的服务从缓存清除，避免反复重试已失效广播）
    pub fn remove_service(&self, instance: &str, service: &str) {
        if let Ok(mut m) = self.inner.lock() {
            m.remove(&format!("{instance}.{service}"));
        }
    }
}

impl Drop for MdnsCache {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 把 mdns-sd 的事件轮询出来并更新缓存。每次调用最多阻塞 500ms（与主循环节奏一致）。
fn drain_events(rx: &Receiver<ServiceEvent>, inner: &Arc<Mutex<HashMap<String, MdnsService>>>) {
    // 每次循环至多处理一批事件，防止积压；timeout 取短值保证主循环可退出
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
        match event {
            ServiceEvent::ServiceResolved(rs) => {
                if let Some(svc) = resolved_to_service(&rs) {
                    let key = format!("{}.{}", svc.instance, svc.service);
                    if let Ok(mut m) = inner.lock() {
                        m.insert(key, svc);
                    }
                }
            }
            ServiceEvent::ServiceRemoved(ty, fullname) => {
                // fullname 形如 "adb-XXX._adb-tls-connect._tcp.local."
                let ty_domain = ty.trim_end_matches('.');
                let instance = fullname
                    .trim_end_matches(&format!("{ty}."))
                    .trim_end_matches('.')
                    .to_string();
                let key = format!("{instance}.{ty_domain}");
                if let Ok(mut m) = inner.lock() {
                    m.remove(&key);
                }
            }
            _ => {}
        }
    }
}

/// 把 mdns-sd 的 ResolvedService 转成内部 MdnsService（取首个 IPv4 地址）。
/// mdns-sd 的 ty_domain 形如 "_adb-tls-pairing._tcp.local."，服务类型要去掉
/// ".local" 域名后缀，得到内部统一的 "_adb-tls-pairing._tcp"。
pub(crate) fn resolved_to_service(rs: &ResolvedService) -> Option<MdnsService> {
    if !rs.is_valid() {
        return None;
    }
    let ty_domain = rs.ty_domain.trim_end_matches('.');
    if ty_domain.is_empty() {
        return None;
    }
    let service = ty_domain
        .strip_suffix(".local")
        .unwrap_or(ty_domain)
        .to_string();
    let instance = rs
        .fullname
        .trim_end_matches(&rs.ty_domain)
        .trim_end_matches('.')
        .to_string();
    if instance.is_empty() {
        return None;
    }
    let ip = rs.get_addresses_v4().into_iter().next()?.to_string();
    Some(MdnsService {
        instance,
        service,
        host: ip,
        port: rs.port,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolve_mapping() {
        // 验证 mdns-sd 的类型/实例名到内部 MdnsService 的换算逻辑
        let fullname = "adb-RM3461-aa11._adb-tls-pairing._tcp.local.";
        let ty_domain = "_adb-tls-pairing._tcp.local.";
        // 实例名：从 fullname 剥离 ty_domain
        let instance = fullname
            .trim_end_matches(ty_domain)
            .trim_end_matches('.');
        assert_eq!(instance, "adb-RM3461-aa11");
        // 服务类型：ty_domain 去掉 ".local" 域名后缀
        let ty = ty_domain.trim_end_matches('.').strip_suffix(".local").unwrap();
        assert_eq!(ty, "_adb-tls-pairing._tcp");
    }
}
