use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::adb::{Adb, DeviceInfo};
use crate::config::Config;
use crate::mdns::MdnsCache;
use crate::pairing::{self, PairingQr};
use crate::scrcpy::{AppInfo, Scrcpy};
use crate::updater;

pub enum Msg {
    Devices(Vec<DeviceInfo>),
    // (请求时的串号, 机主应用(含显示名), [(分身用户id, 用户名, 该用户的应用(含显示名))])
    Packages(String, Vec<AppInfo>, Vec<(i32, String, Vec<AppInfo>)>),
    // (串号, 物理分辨率)
    DeviceSize(String, Option<(u32, u32)>),
    ScrcpyVersion(Option<String>),
    PairDone(Result<String, String>),
    UpdateCheck(Result<updater::ReleaseInfo, String>),
    UpdateInstall(Result<PathBuf, String>),
    Log(String),
}

pub struct GScrcpyApp {
    config: Config,
    adb_path: Option<PathBuf>,
    scrcpy_dir: Option<PathBuf>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    refresh_started: bool,
    stop_refresh: Arc<AtomicBool>,

    devices: Vec<DeviceInfo>,
    selected_serial: Option<String>,
    rename_input: String,
    action_status: String,
    action_error: bool,
    /// 自建 mDNS 缓存（配对/连接服务发现、ip:port 反查串号、设备去重共用）
    mdns: MdnsCache,
    /// 已知设备物理分辨率缓存（wm size）
    device_sizes: HashMap<String, (u32, u32)>,
    /// 已执行过手势重置的设备（每台只重置一次，避免每次刷新重复执行）
    gesture_restored: HashSet<String>,

    /// 机主应用列表（含手机端显示名，来自 scrcpy --list-apps）
    apps: Vec<AppInfo>,
    /// 分身用户应用：(用户id, 用户名, 应用信息(含显示名))
    clone_pkgs: Vec<(i32, String, AppInfo)>,
    /// 当前类名对应的用户（Some=分身用户，None=机主）
    selected_app_user: Option<i32>,
    /// 最近一次成功解析的分身 Activity：(用户id, 包名, component)，用于命令预览
    last_resolved_component: Option<(i32, String, String)>,
    package_filter: String,
    packages_loading: bool,

    app_input: String,
    app_label_input: String,
    resolution_input: String,
    win_w_input: String,
    win_h_input: String,

    logs: Vec<String>,

    qr: Option<PairingQr>,
    qr_texture: Option<egui::TextureHandle>,
    pairing_cancel: Arc<AtomicBool>,
    pairing_status: String,

    manual_ip: String,
    /// 配对码方式手动配对输入
    pair_code_ip: String,
    pair_code: String,

    scrcpy_version: Option<String>,
    latest: Option<Result<updater::ReleaseInfo, String>>,
    update_status: String,
    update_working: bool,
}

impl GScrcpyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cjk_fonts(&cc.egui_ctx);
        let mut config = Config::load();
        // 旧版默认参数迁移：1920x1080(横) + 1080x1920(竖) 比例相反 → 画面只占一部分
        if config.migrate_bad_defaults() {
            config.save();
        }
        let (adb_path, scrcpy_dir) = discover_tools(&config);
        let (tx, rx) = channel();
        let mut app = Self {
            config,
            adb_path,
            scrcpy_dir,
            tx,
            rx,
            refresh_started: false,
            stop_refresh: Arc::new(AtomicBool::new(false)),
            devices: Vec::new(),
            selected_serial: None,
            rename_input: String::new(),
            action_status: String::new(),
            action_error: false,
            mdns: MdnsCache::start(),
            device_sizes: HashMap::new(),
            gesture_restored: HashSet::new(),
            apps: Vec::new(),
            clone_pkgs: Vec::new(),
            selected_app_user: None,
            last_resolved_component: None,
            package_filter: String::new(),
            packages_loading: false,
            app_input: String::new(),
            app_label_input: String::new(),
            resolution_input: String::new(),
            win_w_input: String::new(),
            win_h_input: String::new(),
            logs: Vec::new(),
            qr: None,
            qr_texture: None,
            pairing_cancel: Arc::new(AtomicBool::new(false)),
            pairing_status: String::new(),
            manual_ip: String::new(),
            pair_code_ip: String::new(),
            pair_code: String::new(),
            scrcpy_version: None,
            latest: None,
            update_status: String::new(),
            update_working: false,
        };
        // 初始化输入（分辨率留空 = 直接镜像物理屏幕；窗口留空 = 自动）
        app.app_input = app.config.last_app.clone().unwrap_or_default();
        app.app_label_input = app.config.last_app_label.clone().unwrap_or_default();
        app.resolution_input = app.config.last_resolution.clone().unwrap_or_default();
        app.win_w_input = if app.config.window_width > 0 {
            app.config.window_width.to_string()
        } else {
            String::new()
        };
        app.win_h_input = if app.config.window_height > 0 {
            app.config.window_height.to_string()
        } else {
            String::new()
        };
        app.selected_serial = app.config.last_serial.clone();

        app.ensure_refresh();
        app.ensure_version_check();
        // 默认直接生成配对二维码（无需点击按钮）
        if app.adb_path.is_some() {
            app.start_pairing(&cc.egui_ctx);
        }
        app.log("启动完成。");
        app
    }

    // ---------- 工具发现 ----------

    fn adb(&self) -> Option<Adb> {
        self.adb_path.clone().map(Adb::new)
    }

    fn scrcpy(&self) -> Option<Scrcpy> {
        self.scrcpy_dir.clone().map(|dir| Scrcpy { dir })
    }

    fn tools_dir(&self) -> PathBuf {
        if let Some(dir) = &self.config.scrcpy_dir {
            if let Some(parent) = dir.parent() {
                return parent.to_path_buf();
            }
        }
        current_exe_dir().join("tools")
    }

    fn ensure_refresh(&mut self) {
        if self.refresh_started {
            return;
        }
        let Some(adb_path) = self.adb_path.clone() else { return };
        self.refresh_started = true;
        let tx = self.tx.clone();
        let stop = self.stop_refresh.clone();
        std::thread::spawn(move || {
            let adb = Adb::new(adb_path);
            while !stop.load(Ordering::Relaxed) {
                let devs = adb.devices();
                let _ = tx.send(Msg::Devices(devs));
                for _ in 0..30 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
    }

    fn ensure_version_check(&mut self) {
        let Some(scrcpy_dir) = self.scrcpy_dir.clone() else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let ver = Scrcpy { dir: scrcpy_dir }.version();
            let _ = tx.send(Msg::ScrcpyVersion(ver));
        });
    }

    // ---------- 工具 ----------

    fn log(&mut self, msg: impl Into<String>) {
        self.logs.push(msg.into());
        if self.logs.len() > 300 {
            self.logs.drain(0..self.logs.len() - 300);
        }
    }

    fn copy_text(&mut self, text: &str) {
        match arboard::Clipboard::new() {
            Ok(mut clip) => {
                if let Err(e) = clip.set_text(text.to_string()) {
                    self.action_status = format!("复制到剪贴板失败: {e}");
                    self.action_error = true;
                } else {
                    self.action_status = format!("已复制到剪贴板: {text}");
                    self.action_error = false;
                }
            }
            Err(e) => {
                self.action_status = format!("剪贴板不可用: {e}");
                self.action_error = true;
            }
        }
    }

    fn device_display(&mut self, serial: &str) -> String {
        let model = self
            .devices
            .iter()
            .find(|d| d.serial == serial)
            .and_then(|d| d.model.clone());
        // 手动连接/断开重连后设备以 ip:port 出现，别名仍挂在 mDNS 串号上，
        // 尝试反查 mDNS 实例名以复用别名
        if is_ip_serial(serial) {
            if let Some(mdns) = self.ip_to_mdns_serial(serial) {
                return self.config.display_name(&mdns, model.as_deref());
            }
        }
        self.config.display_name(serial, model.as_deref())
    }

    /// 通过自建 mDNS 缓存把 ip:port 反查为 mDNS 串号
    /// （形如 "adb-XXX._adb-tls-connect._tcp"，与 adb devices -l 里的串号一致）
    fn ip_to_mdns_serial(&self, ip_port: &str) -> Option<String> {
        self.mdns
            .ip_to_instance(ip_port)
            .map(|instance| format!("{instance}.{}", crate::mdns::CONNECT_TYPE))
    }

    fn refresh_once(&mut self) {
        let Some(adb_path) = self.adb_path.clone() else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let adb = Adb::new(adb_path);
            let _ = tx.send(Msg::Devices(adb.devices()));
        });
    }

    fn load_packages(&mut self) {
        let Some(serial) = self.selected_serial.clone() else { return };
        let Some(adb_path) = self.adb_path.clone() else { return };
        let scrcpy_dir = self.scrcpy_dir.clone();
        self.packages_loading = true;
        self.apps.clear();
        self.clone_pkgs.clear();
        self.selected_app_user = None;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let adb = Adb::new(adb_path);
            // 优先用 scrcpy --list-apps（一次返回包名+手机端显示名）；
            // scrcpy 不可用时回退 pm list packages（无名字）
            let apps = match &scrcpy_dir {
                Some(dir) => {
                    let list = Scrcpy { dir: dir.clone() }.list_apps(&serial);
                    if !list.is_empty() {
                        list
                    } else {
                        adb.packages(&serial)
                            .into_iter()
                            .map(|p| AppInfo {
                                name: String::new(),
                                package: p,
                                is_system: false,
                            })
                            .collect()
                    }
                }
                None => adb
                    .packages(&serial)
                    .into_iter()
                    .map(|p| AppInfo {
                        name: String::new(),
                        package: p,
                        is_system: false,
                    })
                    .collect(),
            };
            // 分身用户（华为 user 128「分身应用」/ 努比亚 user 999「应用分身」等）。
            // 与 escrcpy 一致：只列三方应用 + 有桌面启动入口的应用，
            // 显示名直接复用主用户(user 0)里同包名的名字。
            let base_name: HashMap<&str, &str> = apps
                .iter()
                .filter(|a| !a.name.is_empty())
                .map(|a| (a.package.as_str(), a.name.as_str()))
                .collect();
            let mut user_apps: Vec<(i32, String, Vec<AppInfo>)> = Vec::new();
            for u in adb.users(&serial) {
                let pkgs = adb.packages_third_party_for_user(&serial, u.id);
                if pkgs.is_empty() {
                    continue;
                }
                // 只保留有启动入口的应用（query-activities 失败时退回全量）
                let launchable = adb.launchable_packages(&serial, u.id);
                let keep: Vec<String> = if launchable.is_empty() {
                    pkgs
                } else {
                    pkgs.into_iter().filter(|p| launchable.contains(p)).collect()
                };
                if keep.is_empty() {
                    continue;
                }
                let infos = keep
                    .into_iter()
                    .map(|p| AppInfo {
                        name: base_name
                            .get(p.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                        package: p,
                        is_system: false,
                    })
                    .collect();
                user_apps.push((u.id, u.name.clone(), infos));
            }
            let _ = tx.send(Msg::Packages(serial, apps, user_apps));
        });
    }

    fn apply_devices(&mut self, devs: Vec<DeviceInfo>) {
        self.devices = devs;
        // 新出现的 device 状态设备：若开启自动恢复，执行手势重置（每台设备只恢复一次）
        if self.config.restore_gesture {
            let adb_path = self.adb_path.clone();
            for dev in &self.devices {
                if dev.state == "device" && self.gesture_restored.insert(dev.serial.clone()) {
                    let tx = self.tx.clone();
                    let serial = dev.serial.clone();
                    if let Some(adb_path) = adb_path.clone() {
                        std::thread::spawn(move || {
                            let adb = Adb::new(adb_path);
                            let r = adb.restore_gesture_settings(&serial);
                            let msg = format!(
                                "[手势重置] {serial}: {}",
                                r.unwrap_or_else(|e| e)
                            );
                            let _ = tx.send(Msg::Log(msg));
                        });
                    }
                }
            }
        }
        // 同一手机同时出现 mDNS 串号 + ip:port 时，只保留串号条目（ip 条目判为重复）
        let duplicate_ips: HashSet<String> = self
            .devices
            .iter()
            .filter(|d| is_ip_serial(&d.serial))
            .filter_map(|d| {
                let twin = self.ip_to_mdns_serial(&d.serial)?;
                self.devices.iter().any(|x| x.serial == twin).then_some(d.serial.clone())
            })
            .collect();
        // 上次选中的是 ip:port 且已被去重 → 迁移到串号条目
        if let Some(sel) = &self.selected_serial {
            if duplicate_ips.contains(sel) {
                if let Some(twin) = self.ip_to_mdns_serial(sel) {
                    self.selected_serial = Some(twin.clone());
                    self.config.last_serial = Some(twin);
                    self.config.save();
                }
            }
        }
        // 上次选中的设备还在（含迁移后的串号），则保持
        if let Some(sel) = &self.selected_serial {
            if !self.devices.iter().any(|d| &d.serial == sel) {
                self.selected_serial = None;
            }
        }
        // 顺便刷新已知设备分辨率（仅缓存缺失的，避免频繁 adb 调用）
        let known: HashSet<String> = self.device_sizes.keys().cloned().collect();
        let need: Vec<String> = self
            .devices
            .iter()
            .filter(|d| d.state == "device" && !known.contains(&d.serial))
            .map(|d| d.serial.clone())
            .collect();
        if !need.is_empty() {
            let adb_path = self.adb_path.clone();
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let adb = adb_path.map(Adb::new);
                for serial in need {
                    let size = adb.as_ref().and_then(|a| a.wm_size(&serial));
                    let _ = tx.send(Msg::DeviceSize(serial, size));
                }
            });
        }
    }

    /// 该串号是否在设备列表中被判为重复（同一手机的 ip:port 条目）
    fn is_duplicate_serial(&self, serial: &str) -> bool {
        if !is_ip_serial(serial) {
            return false;
        }
        match self.ip_to_mdns_serial(serial) {
            Some(twin) => self.devices.iter().any(|x| x.serial == twin),
            None => false,
        }
    }

    /// 设备分区：主列表显示，还是折叠到"已过滤"区。
    /// 判据：模拟器、本机回环、IP 格式设备（默认屏蔽，可开关）。
    fn is_filtered_device(&self, serial: &str) -> bool {
        if serial.starts_with("emulator-") || serial.starts_with("127.0.0.1:") {
            return true;
        }
        if is_ip_serial(serial) {
            // 重复条目永远隐藏（只显示串号）；纯 IP 设备按开关决定
            if self.is_duplicate_serial(serial) {
                return true;
            }
            return self.config.hide_ip_devices;
        }
        false
    }

    // ---------- 设备操作 ----------

    fn action_connect(&mut self, serial: &str) {
        let Some(adb) = self.adb() else {
            self.action_status = "未找到 adb，请先安装 scrcpy".into();
            self.action_error = true;
            return;
        };
        let target = if serial.contains(':') {
            Some(serial.to_string())
        } else if serial.starts_with("adb-") {
            // mDNS 串号 -> 解析为 ip:port（自建 mDNS 缓存）
            self.mdns
                .services(crate::mdns::CONNECT_TYPE)
                .into_iter()
                .find(|s| {
                    format!("{}.{}", s.instance, s.service) == serial
                })
                .map(|s| format!("{}:{}", s.host, s.port))
        } else {
            None
        };
        match target {
            Some(hp) => match adb.connect(&hp) {
                Ok(o) => {
                    self.action_status = format!("连接 {hp}：{o}");
                    self.action_error = false;
                }
                Err(e) => {
                    self.action_status = format!("连接失败: {e}");
                    self.action_error = true;
                }
            },
            None => {
                self.action_status = "该设备无需手动连接（USB 设备已连接）".into();
                self.action_error = false;
            }
        }
        self.refresh_once();
    }

    fn action_disconnect(&mut self, serial: &str) {
        let Some(adb) = self.adb() else {
            self.action_status = "未找到 adb，请先安装 scrcpy".into();
            self.action_error = true;
            return;
        };
        // 只有网络连接（ip:port 或 mDNS 串号）可以断开；USB/模拟器无需断开
        let is_network = serial.contains(':') || serial.starts_with("adb-");
        if !is_network {
            self.action_status = "本地/USB 设备无需断开（断开仅用于无线/网络连接）".into();
            self.action_error = false;
            return;
        }
        match adb.disconnect(serial) {
            Ok(o) => {
                self.action_status = format!("已断开 {serial}：{o}");
                self.action_error = false;
            }
            Err(e) => {
                self.action_status = format!("断开失败: {e}");
                self.action_error = true;
            }
        }
        self.refresh_once();
    }

    /// 手动修复手机手势热区（荣耀/华为设备专用）。
    ///
    /// 背景：荣耀 MagicOS 实测，虚拟显示器按非物理分辨率（如 1920x1280）创建后，
    /// SystemUI 会把主屏手势热区（GestureNav）注册成虚拟显示器分辨率，导致手机本机
    /// 部分区域手势失效（底部上滑/右侧滑动无效），且移除虚拟显示器也不恢复。
    /// 本操作创建一次**物理分辨率**虚拟显示器再立即结束，手势热区即重新注册为
    /// 物理屏幕尺寸并固化（此后无论投屏是否继续、是否移除，热区保持），
    /// 手机手势恢复，无需重启手机。用户发现"触控/手势不正常"时手动点击执行。
    fn action_repair_gesture(&mut self, serial: &str) {
        let Some(adb) = self.adb() else {
            self.action_status = "未找到 adb，请先安装 scrcpy".into();
            self.action_error = true;
            return;
        };
        if !adb.is_honor_device(serial) {
            self.action_status = "该设备不是荣耀/华为机型，一般不需要此修复".into();
            self.action_error = false;
            self.log("跳过手势热区修复（非荣耀/华为设备）");
            return;
        }
        let Some((w, h)) = adb.wm_size(serial) else {
            self.action_status = "无法获取设备物理分辨率".into();
            self.action_error = true;
            return;
        };
        let Some(scrcpy) = self.scrcpy() else {
            self.action_status = "未找到 scrcpy，请先设置 scrcpy 目录".into();
            self.action_error = true;
            return;
        };
        self.action_status = format!("正在修复手势热区（用物理分辨率 {w}x{h} 覆盖一次）...");
        self.action_error = false;
        self.log(format!("修复手势热区: 物理分辨率 {w}x{h}"));
        match scrcpy.repair_gesture_hotzone(serial, w, h) {
            Ok(()) => {
                self.action_status =
                    "手势热区已物理化修复：手机本机手势已恢复正常（投屏中/结束后均保持，无需重启手机）".into();
                self.action_error = false;
                self.log("手势热区物理化修复完成");
            }
            Err(e) => {
                self.action_status = format!("手势热区修复失败: {e}");
                self.action_error = true;
                self.log(format!("手势热区修复失败: {e}"));
            }
        }
    }

    fn save_rename(&mut self, serial: &str) {
        let name = self.rename_input.trim().to_string();
        if name.is_empty() {
            self.config.device_aliases.remove(serial);
        } else {
            self.config
                .device_aliases
                .insert(serial.to_string(), name.clone());
        }
        self.config.save();
        self.action_status = format!(
            "已重命名: {} -> {}",
            serial,
            if name.is_empty() { "（清除别名）" } else { &name }
        );
        self.action_error = false;
    }

    // ---------- 二维码配对 ----------

    fn start_pairing(&mut self, ctx: &egui::Context) {
        let Some(adb_path) = self.adb_path.clone() else {
            self.log("未找到 adb，请先安装 scrcpy");
            return;
        };
        // 先取消上一次配对线程（防止旧线程继续等到超时）
        self.pairing_cancel.store(true, Ordering::Relaxed);
        self.pairing_cancel = Arc::new(AtomicBool::new(false));
        let qr = pairing::generate();
        match pairing::qr_pixels(&qr.payload, 8) {
            Ok((w, h, px)) => {
                let img = egui::ColorImage::from_rgb([w, h], &px);
                self.qr_texture = Some(ctx.load_texture(
                    "pairing_qr",
                    img,
                    egui::TextureOptions::NEAREST,
                ));
            }
            Err(e) => {
                self.log(format!("生成二维码失败: {e}"));
                return;
            }
        }
        self.qr = Some(qr.clone());
        self.pairing_status = "等待手机扫码配对…（2 分钟内有效）".into();
        self.log(format!(
            "已生成配对二维码（密码 {}）。请在同一 WiFi 下，打开手机「开发者选项 → 无线调试 → 使用二维码配对设备」扫码。",
            qr.password
        ));

        let tx = self.tx.clone();
        let cancel = self.pairing_cancel.clone();
        let mdns = self.mdns.clone();
        std::thread::spawn(move || {
            let log_tx = tx.clone();
            let log = move |msg: &str| {
                let _ = log_tx.send(Msg::Log(msg.to_string()));
            };
            let r = pairing::pair_loop(adb_path, &qr, &cancel, &mdns, &log);
            let _ = tx.send(Msg::PairDone(r));
        });
    }

    /// 配对码方式手动配对（二维码失败的可靠兜底）
    fn action_pair_with_code(&mut self) {
        let hp = self.pair_code_ip.trim().to_string();
        let code = self.pair_code.trim().to_string();
        if hp.is_empty() || code.is_empty() {
            self.action_status = "请输入 ip:port 与 6 位配对码（手机「无线调试 → 使用配对码配对设备」页面显示）".into();
            self.action_error = true;
            return;
        }
        let Some(adb_path) = self.adb_path.clone() else {
            self.action_status = "未找到 adb，请先安装 scrcpy".into();
            self.action_error = true;
            return;
        };
        self.pairing_cancel.store(true, Ordering::Relaxed);
        self.pairing_cancel = Arc::new(AtomicBool::new(false));
        self.log(format!("开始配对码配对: {hp}"));
        let tx = self.tx.clone();
        let cancel = self.pairing_cancel.clone();
        let mdns = self.mdns.clone();
        std::thread::spawn(move || {
            let log_tx = tx.clone();
            let log = move |msg: &str| {
                let _ = log_tx.send(Msg::Log(msg.to_string()));
            };
            let r = pairing::pair_manual(adb_path, &hp, &code, &cancel, &mdns, &log);
            let _ = tx.send(Msg::PairDone(r));
        });
    }

    // ---------- 启动 scrcpy ----------

    fn launch_scrcpy(&mut self) {
        let Some(serial) = self.selected_serial.clone() else {
            self.log("请先选择设备");
            return;
        };
        let Some(scrcpy) = self.scrcpy() else {
            self.log("未找到 scrcpy，请先安装/更新 scrcpy");
            return;
        };
        let pkg = self.app_input.trim().to_string();
        if pkg.is_empty() {
            self.log("请填写应用类名（包名）");
            return;
        }
        let res = self.resolution_input.trim().to_string();
        if !res.is_empty() && !valid_resolution(&res) {
            self.log("分辨率格式应为 宽x高，例如 1920x1080（留空 = 直接镜像物理屏幕）");
            return;
        }
        let ww: u32 = self.win_w_input.trim().parse().unwrap_or(0);
        let wh: u32 = self.win_h_input.trim().parse().unwrap_or(0);
        let label = self.app_label_input.trim().to_string();
        let title = format!(
            "{} - {}",
            if label.is_empty() {
                pkg.as_str()
            } else {
                label.as_str()
            },
            self.device_display(&serial)
        );
        // 诊断：设备物理分辨率 vs 请求分辨率 vs 窗口比例
        let phys = self.device_sizes.get(&serial).copied();
        let mut diag = String::from("画面参数: ");
        if let Some((pw, ph)) = phys {
            diag += &format!("设备物理分辨率 {pw}x{ph}");
            if !res.is_empty() {
                if let Some((rw, rh)) = res.split_once('x').and_then(|(a, b)| {
                    a.parse::<u32>().ok().zip(b.parse::<u32>().ok())
                }) {
                    if rw * ph != rh * pw {
                        diag += &format!(
                            "；请求虚拟分辨率 {res} 与设备比例不一致，app 可能只占画面一部分（四周黑边），建议留空直接镜像或点击「用设备分辨率」"
                        );
                    }
                }
            }
        } else {
            diag += "（未获取到设备物理分辨率）";
            if !res.is_empty() {
                diag += &format!("；请求虚拟分辨率 {res}");
            }
        }
        if ww > 0 && wh > 0 {
            diag += &format!("；窗口 {ww}x{wh}（与视频比例不同会产生黑边）");
        } else {
            diag += "；窗口自动匹配";
        }
        self.log(diag);
        // 启动前重置手势/触控设置（修复无线投屏后侧滑、点按失效；连接时也会自动执行一次，
        // 这里在每次启动前再执行，防止设备重连/系统改动后手势又被切回三键）
        let Some(adb) = self.adb() else {
            self.action_status = "未找到 adb".into();
            self.action_error = true;
            return;
        };
        match adb.restore_gesture_settings(&serial) {
            Ok(o) => self.log(o),
            Err(e) => self.log(format!("重置手势设置失败: {e}")),
        }

        // 分身场景：
        //   A) 虚拟显示器模式（默认，clone_direct_mirror=false，对齐 escrcpy）：
        //      1) resolve-activity 解析分身 Activity（必须 --brief --components，MagicOS 实测）
        //      2) 先启动 scrcpy --new-display 创建虚拟显示器，从输出解析出 displayId
        //      3) am start-activity --user <id> --display <displayId> -n <component>
        //      应用渲染到 scrcpy 窗口对应的虚拟显示器，手机主屏不受影响。
        //      但 Android 系统手势层（边缘返回/底部上滑）只监听主显示器，
        //      虚拟显示器投屏里系统手势不可用（scrcpy/escrcpy 均如此），用快捷键代替。
        //   B) 直接镜像模式（clone_direct_mirror=true）：
        //      分身应用在手机前台启动（真屏），scrcpy 直接镜像主屏，
        //      系统手势可用，但手机屏幕会被应用占用。
        let clone_user = self.selected_app_user;
        if let Some(uid) = clone_user {
            let component = match adb.resolve_activity(&serial, uid, &pkg) {
                Ok(c) => c,
                Err(e) => {
                    // 带真实原因，避免误导为"未创建分身"
                    let msg = format!("分身(user {uid})解析启动 Activity 失败: {e}");
                    self.action_status = msg.clone();
                    self.action_error = true;
                    self.log(msg);
                    return;
                }
            };
            self.last_resolved_component = Some((uid, pkg.clone(), component.clone()));

            if self.config.clone_direct_mirror {
                // 直接镜像模式：先在手机前台启动分身，再镜像主屏
                match adb.start_app_for_user(&serial, uid, &component) {
                    Ok(o) => self.log(format!("已在分身(user {uid})启动 {component}：{o}")),
                    Err(e) => {
                        self.action_status = format!("分身启动失败: {e}");
                        self.action_error = true;
                        return;
                    }
                }
                // 直接镜像：不建虚拟显示器，虚拟分辨率强制留空（忽略用户填的分辨率）
                let args = scrcpy.build_args(&serial, None, "", ww, wh, &title);
                self.log(format!(
                    "启动: {} {}",
                    scrcpy.exe().display(),
                    args.join(" ")
                ));
                match scrcpy.launch(&args) {
                    Ok(()) => {
                        self.action_status = "直接镜像已启动：分身应用在手机前台运行（手机屏幕被占用），系统手势可用".into();
                        self.action_error = false;
                        self.log("scrcpy 已启动（直接镜像主屏）");
                    }
                    Err(e) => {
                        self.action_status = format!("启动失败: {e}");
                        self.action_error = true;
                        self.log(e);
                    }
                }
            } else {
                // 虚拟显示器模式
                let args = scrcpy.build_clone_args(&serial, &res, ww, wh, &title);
                self.log(format!(
                    "启动: {} {}",
                    scrcpy.exe().display(),
                    args.join(" ")
                ));
                let display_id = match scrcpy.launch_with_new_display(&args) {
                    Ok(id) => id,
                    Err(e) => {
                        let msg = format!("创建虚拟显示器失败（未启动分身应用）: {e}");
                        self.action_status = msg.clone();
                        self.action_error = true;
                        self.log(msg);
                        return;
                    }
                };
                self.log(format!(
                    "虚拟显示器已创建 id={display_id}，把分身应用投到该显示器（手机屏幕不受影响）"
                ));
                match adb.start_app_for_user_on_display(&serial, uid, &component, display_id) {
                    Ok(o) => self.log(format!(
                        "已在分身(user {uid})于显示器 {display_id} 启动 {component}：{o}"
                    )),
                    Err(e) => {
                        self.action_status = format!("分身投屏启动失败: {e}");
                        self.action_error = true;
                        return;
                    }
                }
                self.action_status = "分身已投屏到 scrcpy 虚拟显示器（手机屏幕不受影响）。系统手势不可用（Android 平台限制）：鼠标右键=返回，Alt/Super+H=桌面，Alt/Super+S=最近任务".into();
                self.action_error = false;
            }
        } else {
            let args = scrcpy.build_args(&serial, Some(pkg.as_str()), &res, ww, wh, &title);
            // 机主应用同样受虚拟显示器机制影响：填了分辨率=虚拟显示器投屏（系统手势不可用），留空=直接镜像（手势可用）
            let virtual_display = args.iter().any(|a| a.contains("--new-display"));
            self.log(format!(
                "启动: {} {}",
                scrcpy.exe().display(),
                args.join(" ")
            ));
            match scrcpy.launch(&args) {
                Ok(()) => {
                    self.action_status = if virtual_display {
                        "scrcpy 已启动（虚拟显示器模式，手机屏幕不受影响）。系统手势不可用（Android 平台限制）：鼠标右键=返回，Alt/Super+H=桌面，Alt/Super+S=最近任务".into()
                    } else {
                        "scrcpy 已启动（直接镜像模式，系统手势可用）".into()
                    };
                    self.action_error = false;
                    self.log("scrcpy 已启动（详细日志见 scrcpy.log）");
                }
                Err(e) => {
                    self.action_status = format!("启动失败: {e}");
                    self.action_error = true;
                    self.log(e);
                }
            }
        }

        // 持久化
        Config::push_unique(&mut self.config.app_history, pkg.clone(), 20);
        if !label.is_empty() {
            self.config
                .app_labels
                .insert(pkg.clone(), label.clone());
        }
        Config::push_unique(&mut self.config.resolutions, res.clone(), 10);
        self.config.last_app = Some(pkg);
        self.config.last_resolution = Some(res);
        self.config.window_width = ww;
        self.config.window_height = wh;
        self.config.last_app_label = if label.is_empty() { None } else { Some(label) };
        self.config.last_serial = Some(serial);
        self.config.save();
    }

    // ---------- 更新 scrcpy ----------

    fn check_update(&mut self) {
        if self.update_working {
            return;
        }
        self.update_working = true;
        self.update_status = "正在检查更新…".into();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = updater::latest_release();
            let _ = tx.send(Msg::UpdateCheck(r));
        });
    }

    fn install_latest(&mut self) {
        if self.update_working {
            return;
        }
        self.update_working = true;
        self.update_status = "正在获取最新版本信息…".into();
        let tools = self.tools_dir();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = match updater::latest_release() {
                Ok(info) => {
                    let _ = tx.send(Msg::Log(format!(
                        "正在下载并安装 {}（视网速可能需要几分钟）…",
                        info.tag
                    )));
                    updater::install_new_version(&tools, &info)
                }
                Err(e) => Err(e),
            };
            let _ = tx.send(Msg::UpdateInstall(r));
        });
    }

    // ---------- 消息处理 ----------

    fn poll_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Devices(devs) => self.apply_devices(devs),
                Msg::Packages(serial, apps, user_pkgs) => {
                    if self.selected_serial.as_deref() == Some(serial.as_str()) {
                        self.apps = apps;
                        self.clone_pkgs = user_pkgs
                            .iter()
                            .flat_map(|(id, uname, ps)| {
                                ps.iter().map(move |p| (*id, uname.clone(), p.clone()))
                            })
                            .collect();
                        self.packages_loading = false;
                        let n = self.clone_pkgs.len();
                        self.log(format!(
                            "已加载 {} 个应用（含 {} 个分身应用）",
                            self.apps.len() + n,
                            n
                        ));
                    }
                }
                Msg::ScrcpyVersion(v) => self.scrcpy_version = v,
                Msg::DeviceSize(serial, size) => {
                    if let Some(sz) = size {
                        self.device_sizes.insert(serial.clone(), sz);
                        self.log(format!(
                            "[设备] {serial} 物理分辨率 {0}x{1}",
                            sz.0, sz.1
                        ));
                    }
                }
                Msg::PairDone(r) => {
                    self.pairing_cancel = Arc::new(AtomicBool::new(true));
                    match r {
                        Ok(s) => {
                            self.pairing_status = s.clone();
                            self.log(s);
                            self.refresh_once();
                        }
                        Err(e) => {
                            self.pairing_status = e.clone();
                            self.log(e);
                        }
                    }
                }
                Msg::UpdateCheck(r) => {
                    self.update_working = false;
                    self.latest = Some(r.clone());
                    match r {
                        Ok(info) => {
                            let cur = self
                                .scrcpy_version
                                .as_deref()
                                .and_then(|v| v.split_whitespace().nth(1))
                                .unwrap_or("?");
                            if updater::version_cmp(cur, &info.tag) == std::cmp::Ordering::Less {
                                self.update_status = format!("发现新版本 {}（当前 {}）", info.tag, cur);
                            } else {
                                self.update_status = format!("已是最新版本（{}）", info.tag);
                            }
                        }
                        Err(e) => {
                            self.update_status = e.clone();
                            self.log(e);
                        }
                    }
                }
                Msg::UpdateInstall(r) => {
                    self.update_working = false;
                    match r {
                        Ok(dir) => {
                            self.config.scrcpy_dir = Some(dir.clone());
                            self.config.save();
                            self.adb_path = Some(dir.join("adb.exe"));
                            self.scrcpy_dir = Some(dir);
                            self.ensure_refresh();
                            self.ensure_version_check();
                            self.update_status = "安装完成".into();
                            self.log("scrcpy 安装/更新完成");
                        }
                        Err(e) => {
                            self.update_status = format!("更新失败: {e}");
                            self.log(e);
                        }
                    }
                }
                Msg::Log(s) => self.log(s),
            }
        }
    }

    // ---------- UI ----------

    /// 右侧「启动 scrcpy」面板内的版本/更新信息：scrcpy/adb 版本、检查更新、更新状态与最近日志
    fn ui_bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "scrcpy: {}",
                    self.scrcpy_version
                        .as_deref()
                        .and_then(|v| v.split_whitespace().nth(1))
                        .unwrap_or("未知")
                ))
                .small(),
            );
            if ui.button("检查更新").clicked() {
                self.check_update();
            }
            if self.update_working {
                ui.spinner();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "adb: {}",
                    self.adb_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "未找到".into())
                ))
                .small(),
            );
        });
        if !self.update_status.is_empty() {
            ui.label(egui::RichText::new(&self.update_status).small());
        }
        if let Some(Ok(info)) = &self.latest {
            let cur = self
                .scrcpy_version
                .as_deref()
                .and_then(|v| v.split_whitespace().nth(1))
                .unwrap_or("?");
            if updater::version_cmp(cur, &info.tag) == std::cmp::Ordering::Less {
                if ui.button(format!("下载并更新到 {}", info.tag)).clicked() {
                    self.install_latest();
                }
            }
        }
        if let Some(last) = self.logs.last() {
            ui.label(egui::RichText::new(format!("日志: {last}")).small().weak());
        }
    }

    fn ui_devices(&mut self, ui: &mut egui::Ui) {
        ui.heading("设备");
        if self.adb_path.is_none() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 120, 120),
                "未找到 adb / scrcpy",
            );
            ui.label("点击下方按钮下载 scrcpy（内含 adb）：");
            if ui
                .button("下载并安装 scrcpy（含 adb）")
                .clicked()
            {
                self.install_latest();
            }
            if !self.update_status.is_empty() {
                ui.label(&self.update_status);
            }
            return;
        }

        ui.horizontal(|ui| {
            if ui.button("刷新").clicked() {
                self.refresh_once();
            }
            ui.label(format!("自动刷新 · 共 {} 台", self.devices.len()));
        });
        if ui
            .checkbox(&mut self.config.restore_gesture, "连接后自动重置手势导航")
            .on_hover_text("修复部分机型（如华为）无线调试连接后侧滑/从底部滑动失效；每台设备连接时自动执行一次")
            .changed()
        {
            self.config.save();
        }
        if ui
            .checkbox(&mut self.config.hide_ip_devices, "屏蔽 IP 格式设备（同手机只显示串号）")
            .on_hover_text("同一手机在 adb 里会同时出现 mDNS 串号与 ip:port 两个条目，默认只显示串号；没有对应串号的纯 IP 设备放进下方折叠区。取消勾选后全部显示。")
            .changed()
        {
            self.config.save();
            self.refresh_once();
        }

        let mut to_select: Option<String> = None;
        // 主列表过滤掉模拟器、本机回环、IP 格式设备（默认屏蔽），被过滤的单独展示在折叠区
        let (main_devs, filtered_devs): (Vec<&DeviceInfo>, Vec<&DeviceInfo>) = self
            .devices
            .iter()
            .partition(|d| !self.is_filtered_device(&d.serial));
        for dev in &main_devs {
            if render_device_row(ui, &self.config, self.selected_serial.as_deref(), dev)
                .clicked()
            {
                to_select = Some(dev.serial.clone());
            }
        }
        if !filtered_devs.is_empty() {
            egui::CollapsingHeader::new(format!("已过滤 {} 台", filtered_devs.len()))
                .default_open(false)
                .show(ui, |ui| {
                    for dev in &filtered_devs {
                        if render_device_row(ui, &self.config, self.selected_serial.as_deref(), dev)
                            .clicked()
                        {
                            to_select = Some(dev.serial.clone());
                        }
                    }
                });
        }
        if let Some(serial) = to_select {
            if self.selected_serial.as_deref() != Some(serial.as_str()) {
                self.selected_serial = Some(serial.clone());
                self.config.last_serial = Some(serial);
                self.config.save();
                self.apps.clear();
                self.clone_pkgs.clear();
                self.selected_app_user = None;
                self.load_packages();
            }
        }

        if let Some(serial) = self.selected_serial.clone() {
            ui.separator();
            ui.label(format!("选中: {}", self.device_display(&serial)));
            let is_device = self
                .devices
                .iter()
                .any(|d| &d.serial == &serial && d.state == "device");
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !is_device,
                        egui::Button::new(if is_device { "已连接" } else { "连接" }),
                    )
                    .clicked()
                {
                    self.action_connect(&serial);
                }
                if ui.button("断开").clicked() {
                    self.action_disconnect(&serial);
                }
                let repair_btn = ui.add_enabled(
                    is_device,
                    egui::Button::new("修复手机手势"),
                );
                if repair_btn
                    .on_hover_text("荣耀/华为设备用虚拟显示器投屏后，若手机本机手势失效（底部上滑/右侧滑动无效），点击此按钮：自动用设备物理分辨率创建一次虚拟显示器再移除，手势热区即恢复为物理尺寸，无需重启手机。")
                    .clicked()
                {
                    self.action_repair_gesture(&serial);
                }
                if ui.button("复制串号").clicked() {
                    self.copy_text(&serial);
                }
            });
            if !self.action_status.is_empty() {
                let color = if self.action_error {
                    egui::Color32::from_rgb(255, 110, 110)
                } else {
                    egui::Color32::from_rgb(150, 190, 255)
                };
                ui.colored_label(color, &self.action_status);
            }
            ui.horizontal(|ui| {
                ui.label("重命名:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.rename_input)
                        .hint_text("输入设备别名"),
                );
                if ui.button("保存").clicked() {
                    self.save_rename(&serial);
                }
            });
        }

        ui.separator();
        ui.heading("无线调试配对");
        if self.qr.is_none() {
            ui.label("二维码未生成（未找到 adb 或生成失败）");
            if ui.button("生成配对二维码").clicked() {
                let ctx = ui.ctx().clone();
                self.start_pairing(&ctx);
            }
        } else {
            ui.horizontal(|ui| {
                if ui.button("重新生成").clicked() {
                    self.qr = None;
                    self.qr_texture = None;
                    self.pairing_status.clear();
                    let ctx = ui.ctx().clone();
                    self.start_pairing(&ctx);
                }
            });
            if let Some(tex) = &self.qr_texture {
                ui.add(egui::Image::new((tex.id(), egui::vec2(230.0, 230.0))));
            }
            if let Some(qr) = &self.qr {
                ui.monospace(format!("配对密码: {}", qr.password));
            }
            if !self.pairing_status.is_empty() {
                ui.label(&self.pairing_status);
            }
        }

        ui.separator();
        ui.label("手动连接（已配对过可直接输入）:");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.manual_ip)
                    .hint_text("如 192.168.1.5:37855"),
            );
            if ui.button("连接").clicked() {
                let hp = self.manual_ip.trim().to_string();
                if hp.is_empty() {
                    self.action_status = "请输入 ip:port".into();
                    self.action_error = true;
                } else if let Some(adb) = self.adb() {
                    match adb.connect(&hp) {
                        Ok(o) => {
                            self.action_status = format!("连接 {hp}：{o}");
                            self.action_error = false;
                        }
                        Err(e) => {
                            self.action_status = format!("连接失败: {e}");
                            self.action_error = true;
                        }
                    }
                    self.refresh_once();
                }
            }
        });

        ui.separator();
        ui.label("配对码方式（二维码失败时用）:");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.pair_code_ip)
                    .hint_text("手机无线调试页的 ip:port，如 192.168.1.5:43129"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.pair_code)
                    .hint_text("6 位配对码")
                    .desired_width(90.0),
            );
            if ui.button("配对并连接").clicked() {
                self.action_pair_with_code();
            }
        });

        egui::CollapsingHeader::new("配对过程日志（排障用）")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "二维码配对每步都会记录真实输出/错误；若仍未配对成功，把这里的内容发给我即可定位问题。",
                    )
                    .small()
                    .weak(),
                );
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for l in self.logs.iter().rev().take(80) {
                            ui.label(egui::RichText::new(l).small());
                        }
                    });
            });
    }

    fn ui_launch(&mut self, ui: &mut egui::Ui) {
        ui.heading("启动 scrcpy");
        self.ui_bottom_bar(ui);
        ui.separator();
        let serial = self.selected_serial.clone();
        if let Some(s) = &serial {
            ui.label(format!("设备: {}", self.device_display(s)));
        } else {
            ui.colored_label(egui::Color32::from_rgb(255, 160, 80), "请先在左侧选择设备");
        }

        ui.horizontal(|ui| {
            ui.label("应用类名(包名):");
            let sel_user = self.selected_app_user;
            egui::ComboBox::from_id_salt("app_history")
                .selected_text(if self.app_input.is_empty() {
                    "历史记录…".to_string()
                } else {
                    self.app_input.clone()
                })
                .width(220.0)
                .show_ui(ui, |ui| {
                    if self.config.app_history.is_empty() {
                        ui.label("（暂无历史）");
                    }
                    for item in self.config.app_history.clone() {
                        if ui.selectable_label(self.app_input == item, &item).clicked() {
                            self.app_input = item;
                            self.selected_app_user = None;
                        }
                    }
                });
            let app_resp = ui.add(
                egui::TextEdit::singleline(&mut self.app_input)
                    .hint_text("如 com.gof.china")
                    .desired_width(260.0),
            );
            if app_resp.changed() {
                // 手动输入/修改时视为机主应用
                self.selected_app_user = None;
            }
            if let Some(uid) = sel_user {
                ui.label(
                    egui::RichText::new(format!("分身 user {uid}"))
                        .small()
                        .color(egui::Color32::from_rgb(255, 190, 90)),
                );
            }
        });
        ui.horizontal(|ui| {
            ui.label("显示名(可选):");
            ui.add(
                egui::TextEdit::singleline(&mut self.app_label_input)
                    .hint_text("用于窗口标题，如 王者荣耀")
                    .desired_width(260.0),
            );
        });
        ui.horizontal(|ui| {
            ui.label("分辨率:");
            egui::ComboBox::from_id_salt("res_history")
                .selected_text(if self.resolution_input.is_empty() {
                    "直接镜像物理屏幕".to_string()
                } else {
                    self.resolution_input.clone()
                })
                .width(170.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.resolution_input.is_empty(), "直接镜像物理屏幕")
                        .clicked()
                    {
                        self.resolution_input.clear();
                    }
                    for item in self.config.resolutions.clone() {
                        if ui
                            .selectable_label(self.resolution_input == item, &item)
                            .clicked()
                        {
                            self.resolution_input = item;
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.resolution_input)
                    .hint_text("留空=直接镜像")
                    .desired_width(110.0),
            );
            let size = self
                .selected_serial
                .clone()
                .and_then(|s| self.device_sizes.get(&s).copied());
            if let Some((w, h)) = size {
                if ui
                    .button(format!("用设备分辨率 {w}x{h}"))
                    .on_hover_text("把虚拟分辨率设为设备物理分辨率，避免比例不一致导致 app 只占画面一部分")
                    .clicked()
                {
                    self.resolution_input = format!("{w}x{h}");
                }
            }
            ui.label("窗口:");
            ui.add(
                egui::TextEdit::singleline(&mut self.win_w_input)
                    .desired_width(60.0)
                    .hint_text("留空=自动"),
            );
            ui.label("x");
            ui.add(
                egui::TextEdit::singleline(&mut self.win_h_input)
                    .desired_width(60.0)
                    .hint_text("留空=自动"),
            );
        });
        ui.label(
            egui::RichText::new(
                "提示：分辨率/窗口留空 = 直接镜像物理屏幕、窗口自动匹配比例，不会出现 app 只占画面一部分的情况。",
            )
            .small()
            .weak(),
        );

        let title_preview = match &serial {
            Some(s) => {
                let pkg = self.app_input.trim().to_string();
                let label = self.app_label_input.trim().to_string();
                let name = if label.is_empty() { pkg.as_str() } else { label.as_str() };
                if name.is_empty() {
                    "窗口标题: （类名为空）".to_string()
                } else {
                    format!("窗口标题: {name} - {}", self.device_display(s))
                }
            }
            None => "窗口标题: （未选设备）".to_string(),
        };
        ui.label(title_preview);
        // 分身投屏模式选择 + 快捷键提示（仅分身场景显示）
        if self.selected_app_user.is_some() {
            ui.horizontal(|ui| {
                if ui
                    .checkbox(
                        &mut self.config.clone_direct_mirror,
                        "分身直接镜像手机屏幕（不建虚拟显示器）",
                    )
                    .on_hover_text("虚拟显示器模式：分身显示在 scrcpy 窗口，手机屏幕不被占用，但 Android 系统手势（边缘返回/上滑）不可用（平台限制，scrcpy/escrcpy 相同），可用鼠标右键=返回、Alt/Super+H=桌面、Alt/Super+S=最近任务代替。直接镜像模式：分身应用在手机前台启动并镜像，系统手势可用，但手机屏幕会被应用占用。")
                    .changed()
                {
                    self.config.save();
                }
            });
            if !self.config.clone_direct_mirror {
                ui.label(
                    egui::RichText::new(
                        "虚拟显示器模式：系统手势（边缘返回/上滑）不可用——鼠标右键=返回，Alt/Super+H=桌面，Alt/Super+S=最近任务",
                    )
                    .small()
                    .weak(),
                );
            }
        }

        if ui
            .add_enabled(serial.is_some(), egui::Button::new("打开 scrcpy"))
            .clicked()
        {
            self.launch_scrcpy();
        }

        egui::CollapsingHeader::new("查看生成的命令")
            .default_open(false)
            .show(ui, |ui| {
                if let Some(s) = &serial {
                    let pkg = self.app_input.trim().to_string();
                    let res = self.resolution_input.trim().to_string();
                    let ww = self.win_w_input.parse::<u32>().unwrap_or(0);
                    let wh = self.win_h_input.parse::<u32>().unwrap_or(0);
                    let label = self.app_label_input.trim().to_string();
                    let name = if label.is_empty() { pkg.as_str() } else { label.as_str() };
                    let title = format!("{name} - {}", self.device_display(s));
                    let clone_user = self.selected_app_user;
                    if let Some(uid) = clone_user {
                        ui.monospace(format!(
                            "adb -s {s} shell cmd package resolve-activity --brief --components --user {uid} {pkg}  # 解析 Activity（分身必须 --components）"
                        ));
                        if let Some(scrcpy) = self.scrcpy() {
                            let args = scrcpy.build_clone_args(s, &res, ww, wh, &title);
                            ui.monospace(format!("scrcpy {}", args.join(" ")));
                        }
                        // 分身投屏命令：displayId 来自 scrcpy 输出（每次启动动态变化）；
                        // 若已成功解析过同包名 Activity，直接显示真实组件
                        let cached = self
                            .last_resolved_component
                            .as_ref()
                            .filter(|(u, p, _)| *u == uid && p == &pkg)
                            .map(|(_, _, c)| c.clone());
                        match cached {
                            Some(c) => ui.monospace(format!(
                                "adb -s {s} shell am start-activity --user {uid} --display <scrcpy输出的ID> -n {c}"
                            )),
                            None => ui.monospace(format!(
                                "adb -s {s} shell am start-activity --user {uid} --display <scrcpy输出的ID> -n <{pkg}的Activity>  # 点击「打开 scrcpy」后自动解析"
                            )),
                        };
                    } else if let Some(scrcpy) = self.scrcpy() {
                        let args = scrcpy.build_args(s, Some(&pkg), &res, ww, wh, &title);
                        ui.monospace(format!("scrcpy {}", args.join(" ")));
                    }
                }
            });

        ui.separator();
        ui.heading(format!("应用列表（点击复制并填入）"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.package_filter)
                    .hint_text("过滤…")
                    .desired_width(200.0),
            );
            if ui.button("刷新应用列表").clicked() {
                self.load_packages();
            }
            if self.packages_loading {
                ui.spinner();
            }
        });
        let filter = self.package_filter.to_lowercase();
        let filtered: Vec<AppInfo> = self
            .apps
            .iter()
            .filter(|a| {
                filter.is_empty()
                    || a.package.to_lowercase().contains(&filter)
                    || a.name.to_lowercase().contains(&filter)
            })
            .cloned()
            .collect();
        ui.label(format!("共 {} 个（可搜应用名或包名）", filtered.len()));
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for app in filtered {
                    let is_sel =
                        self.app_input == app.package && self.selected_app_user.is_none();
                    let text = if app.name.is_empty() {
                        app.package.clone()
                    } else {
                        format!("{}  {}", app.name, app.package)
                    };
                    if ui.selectable_label(is_sel, text).clicked() {
                        self.copy_text(&app.package);
                        self.app_input = app.package.clone();
                        self.selected_app_user = None;
                        // 手机端显示名自动填入「显示名」，无需手动填写
                        if !app.name.is_empty() {
                            self.app_label_input = app.name.clone();
                            self.config
                                .app_labels
                                .insert(app.package.clone(), app.name.clone());
                            self.config.save();
                        } else if self.app_label_input.is_empty() {
                            self.app_label_input = app.package.clone();
                        }
                        self.log(format!(
                            "已复制并填入: {}{}",
                            app.name,
                            if app.name.is_empty() {
                                String::new()
                            } else {
                                format!("（{}）", app.package)
                            }
                        ));
                    }
                }
                // 分身应用分区（华为 user 128 / 努比亚 user 999 等），按用户分组。
                // 名字复用主用户(user 0)里同包名的显示名（与 escrcpy 一致）。
                let clone_filtered: Vec<(i32, String, AppInfo)> = self
                    .clone_pkgs
                    .iter()
                    .filter(|(_, _, a)| {
                        filter.is_empty()
                            || a.package.to_lowercase().contains(&filter)
                            || a.name.to_lowercase().contains(&filter)
                    })
                    .cloned()
                    .collect();
                if !clone_filtered.is_empty() {
                    ui.separator();
                    egui::CollapsingHeader::new(format!(
                        "分身应用（{} 个）",
                        clone_filtered.len()
                    ))
                    .default_open(true)
                    .show(ui, |ui| {
                        let mut groups: Vec<(i32, String, Vec<AppInfo>)> = Vec::new();
                        for (uid, uname, app) in clone_filtered {
                            match groups.iter_mut().find(|(id, _, _)| *id == uid) {
                                Some((_, _, ps)) => ps.push(app),
                                None => groups.push((uid, uname, vec![app])),
                            }
                        }
                        for (uid, uname, apps) in groups {
                            ui.label(
                                egui::RichText::new(format!("{uname}（user {uid}）"))
                                    .strong()
                                    .small(),
                            );
                            for app in apps {
                                let is_sel =
                                    self.app_input == app.package
                                        && self.selected_app_user == Some(uid);
                                let text = if app.name.is_empty() {
                                    format!("{}  [分身 user {uid}]", app.package)
                                } else {
                                    format!("{}  {}  [分身 user {uid}]", app.name, app.package)
                                };
                                if ui.selectable_label(is_sel, text).clicked() {
                                    self.copy_text(&app.package);
                                    self.app_input = app.package.clone();
                                    self.selected_app_user = Some(uid);
                                    if !app.name.is_empty() {
                                        self.app_label_input = app.name.clone();
                                        self.config
                                            .app_labels
                                            .insert(app.package.clone(), app.name.clone());
                                        self.config.save();
                                    } else if self.app_label_input.is_empty() {
                                        self.app_label_input = app.package.clone();
                                    }
                                    self.log(format!(
                                        "已复制并填入分身: {}{} (user {uid})",
                                        app.name,
                                        if app.name.is_empty() {
                                            String::new()
                                        } else {
                                            format!("（{}）", app.package)
                                        }
                                    ));
                                }
                            }
                        }
                    });
                }
            });
    }
}

impl eframe::App for GScrcpyApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_messages();
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("left")
            .resizable(true)
            .default_size(330.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.ui_devices(ui);
                });
            });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.ui_launch(ui);
            });
        });
    }
}

impl Drop for GScrcpyApp {
    fn drop(&mut self) {
        self.stop_refresh.store(true, Ordering::Relaxed);
        self.config.save();
    }
}

// ---------- 工具函数 ----------

/// 是否为 IP 格式串号（如 "192.168.1.5:37855"）：非 adb- 开头且形如 ip:port。
/// mDNS 串号（adb-XXX._adb-tls-connect._tcp）与 USB/模拟器串号都不算。
fn is_ip_serial(serial: &str) -> bool {
    if serial.starts_with("adb-") || serial.starts_with("emulator-") {
        return false;
    }
    let Some((host, port)) = serial.rsplit_once(':') else {
        return false;
    };
    if host.is_empty() || port.is_empty() {
        return false;
    }
    // 主机部分以数字开头（IPv4）或为 [IPv6]
    host.starts_with(|c: char| c.is_ascii_digit()) || host.starts_with('[')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_serial_predicate() {
        assert!(is_ip_serial("192.168.1.5:37855"));
        assert!(is_ip_serial("10.0.0.8:5555"));
        assert!(is_ip_serial("127.0.0.1:7555")); // 回环也是 ip:port，是否隐藏由过滤逻辑处理
        assert!(!is_ip_serial("emulator-5554"));
        assert!(!is_ip_serial("adb-D1222091020A-aWsoaY._adb-tls-connect._tcp"));
        assert!(!is_ip_serial("127.0.0.1")); // 无端口不算
        assert!(!is_ip_serial("f51065db"));
    }
}

/// 渲染一行设备并返回点击响应。
/// 未选中：扁平文字行（offline 灰色、其余浅色，不使用绿色）；
/// 选中：亮蓝底白字加粗，保证醒目。
fn render_device_row(
    ui: &mut egui::Ui,
    config: &Config,
    selected_serial: Option<&str>,
    dev: &DeviceInfo,
) -> egui::Response {
    let name = config.display_name(&dev.serial, dev.model.as_deref());
    let selected = selected_serial == Some(dev.serial.as_str());
    let mut text = egui::RichText::new(format!("{}  [{}]", name, dev.state));
    if selected {
        text = text.color(egui::Color32::WHITE).strong();
        ui.add(
            egui::Button::new(text)
                .fill(egui::Color32::from_rgb(35, 110, 250))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(140, 180, 255)))
                .corner_radius(4.0),
        )
    } else {
        // 未选中/被过滤设备：浅蓝色（离线稍暗）
        let color = if dev.state == "offline" {
            egui::Color32::from_rgb(110, 145, 190)
        } else {
            egui::Color32::from_rgb(150, 195, 245)
        };
        ui.add(egui::Button::new(text.color(color)).frame(false))
    }
}

/// 加载系统中文字体作为回退字体。
/// egui 内置字体不含 CJK 字形，中文会显示为方块，这里从 Windows 系统字体目录加载
/// 微软雅黑等字体，追加到比例/等宽字体的回退链中。
fn setup_cjk_fonts(ctx: &egui::Context) {
    const CANDIDATES: [&str; 7] = [
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\msyhl.ttc",
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
        r"C:\Windows\Fonts\msjh.ttc",   // 繁体（微软正黑）
    ];
    let data = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok());
    let Some(data) = data else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(data)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

fn valid_resolution(s: &str) -> bool {
    // 空串 = 直接镜像物理屏幕（合法）；否则必须是 宽x高
    if s.is_empty() {
        return true;
    }
    if let Some((w, h)) = s.split_once('x') {
        return w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok();
    }
    false
}

fn current_exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// 按优先级发现工具：配置目录 > exe 旁 tools > PATH
fn discover_tools(config: &Config) -> (Option<PathBuf>, Option<PathBuf>) {
    if let Some(dir) = &config.scrcpy_dir {
        if dir.join("scrcpy.exe").exists() && dir.join("adb.exe").exists() {
            return (
                Some(dir.join("adb.exe")),
                Some(dir.clone()),
            );
        }
    }
    let tools = current_exe_dir().join("tools");
    if let Ok(entries) = std::fs::read_dir(&tools) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("scrcpy.exe").exists() && p.join("adb.exe").exists() {
                return (Some(p.join("adb.exe")), Some(p));
            }
        }
    }
    let adb_on_path = find_on_path("adb.exe").or_else(|| find_on_path("adb"));
    let scrcpy_on_path = find_on_path("scrcpy.exe");
    match (adb_on_path, scrcpy_on_path) {
        (Some(adb), Some(scrcpy)) => {
            if let Some(dir) = scrcpy.parent() {
                return (Some(adb), Some(dir.to_path_buf()));
            }
            (Some(adb), None)
        }
        (Some(adb), None) => (Some(adb), None),
        _ => (None, None),
    }
}
