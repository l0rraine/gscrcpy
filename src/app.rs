use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use eframe::egui;

use crate::adb::{Adb, DeviceInfo, CREATE_NO_WINDOW};
use crate::config::{physical_serial, Config, GestureFixMode, Profile};
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
    /// adb track-devices 检测到设备列表变化（主线程收到后重新拉取详情）
    DevicesTick,
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

    /// 机主应用列表（含手机端显示名，来自 scrcpy --list-apps）
    apps: Vec<AppInfo>,
    /// 分身用户应用：(用户id, 用户名, 应用信息(含显示名))
    clone_pkgs: Vec<(i32, String, AppInfo)>,
    packages_loading: bool,

    /// 当前选中设备的 profile 列表（与 config.profiles 同步，中栏展示）
    profiles: Vec<Profile>,
    /// 当前编辑中的 profile 副本（右栏编辑，改动即保存回 config）
    profile_edit: Option<Profile>,
    /// 最近一次成功解析的分身 Activity：(用户id, 包名, component)，用于命令预览
    last_resolved_component: Option<(i32, String, String)>,

    /// 正在改名的 profile（None=无；列表默认只读，点「改名」才进入编辑）
    renaming_profile: Option<String>,
    /// 刚点「改名」进入编辑态，下一帧给输入框请求焦点（否则无法直接输入）
    rename_focus_requested: bool,
    /// 自建应用下拉弹层是否打开（点击弹层外自动关闭）
    app_combo_open: bool,
    /// 重命名输入框文本（常驻控件，文本存字段保证输入同步）
    renaming_name: String,

    app_input: String,
    app_label_input: String,
    resolution_input: String,
    win_w_input: String,
    win_h_input: String,

    qr: Option<PairingQr>,
    qr_texture: Option<egui::TextureHandle>,
    /// 二维码区"？"帮助是否展开
    qr_help_open: bool,
    /// 二维码是否已失效（配对超时后置 true：二维码变灰、点击即可重新生成）
    qr_expired: bool,
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
        // 手势热区统一为 Auto（每次投屏结束自动修复）；旧配置保持其余字段不变
        if config.migrate_all_gesture_auto() {
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
            apps: Vec::new(),
            clone_pkgs: Vec::new(),
            packages_loading: false,
            profiles: Vec::new(),
            profile_edit: None,
            last_resolved_component: None,
            renaming_profile: None,
            rename_focus_requested: false,
            app_combo_open: false,
            renaming_name: String::new(),
            app_input: String::new(),
            app_label_input: String::new(),
            resolution_input: String::new(),
            win_w_input: String::new(),
            win_h_input: String::new(),
            qr: None,
            qr_texture: None,
            qr_help_open: false,
            qr_expired: false,
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
        // 恢复上次设备；窗口尺寸默认留空（= 与分辨率相同，自动匹配）
        app.selected_serial = app.config.last_serial.clone();
        app.win_w_input = String::new();
        app.win_h_input = String::new();

        app.ensure_refresh();
        app.ensure_version_check();
        // 默认直接生成配对二维码（无需点击按钮）
        if app.adb_path.is_some() {
            app.start_pairing(&cc.egui_ctx);
        }
        // 若已选中设备，初始化其 profile 与应用列表
        if let Some(serial) = app.selected_serial.clone() {
            app.init_device_state(&serial);
        }
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

    // ---------- 刷新（adb track-devices 事件驱动，向 escrcpy 看齐） ----------

    fn ensure_refresh(&mut self) {
        if self.refresh_started {
            return;
        }
        let Some(adb_path) = self.adb_path.clone() else { return };
        self.refresh_started = true;
        let tx = self.tx.clone();
        // 定时刷新线程的独立 sender/stop（须在 move 进 track 线程前 clone）
        let tx_tick = tx.clone();
        let stop_tick = self.stop_refresh.clone();
        let stop = self.stop_refresh.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                // adb track-devices 持续输出设备状态变化（阻塞），断开后自动重连
                let mut cmd = Command::new(&adb_path);
                cmd.arg("track-devices");
                cmd.stdin(Stdio::null());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::null());
                #[cfg(windows)]
                cmd.creation_flags(CREATE_NO_WINDOW);
                let Ok(mut child) = cmd.spawn() else {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                };
                let Some(stdout) = child.stdout.take() else {
                    let _ = child.kill();
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                };
                let reader = std::io::BufReader::new(stdout);
                let mut last_event = Instant::now();
                for line in reader.lines() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(line) = line else { break };
                    let line = line.trim();
                    if line.is_empty() || line.starts_with("List of devices") {
                        continue;
                    }
                    // 设备行变化即触发详情刷新；去抖 500ms，避免连发
                    if last_event.elapsed() >= Duration::from_millis(500) {
                        let _ = tx.send(Msg::DevicesTick);
                        last_event = Instant::now();
                    }
                }
                // track 流结束（adb 退出/设备断开）后稍等重连
                std::thread::sleep(Duration::from_millis(1500));
            }
        });
        // 每 10 秒发一次定时刷新：adb 对模拟器端口开关无感知（track-devices 只在
        // 已连接设备变化时输出），定时探测可让打开模拟器后自动出现在列表
        std::thread::spawn(move || {
            while !stop_tick.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(10));
                let _ = tx_tick.send(Msg::DevicesTick);
            }
        });
        // 启动后立即拉一次设备列表
        self.refresh_once();
    }

    fn refresh_once(&mut self) {
        let Some(adb_path) = self.adb_path.clone() else { return };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let adb = Adb::new(adb_path);
            // 自动探测本机常见模拟器 adb 端口（MuMu 等），
            // 打开模拟器后无需手动 adb connect 即可在列表出现
            try_connect_local_emulators(&adb);
            let _ = tx.send(Msg::Devices(adb.devices()));
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
        if is_ip_serial(serial) {
            if let Some(mdns) = self.ip_to_mdns_serial(serial) {
                return self.config.display_name(&mdns, model.as_deref());
            }
        }
        self.config.display_name(serial, model.as_deref())
    }

    /// 通过自建 mDNS 缓存把 ip:port 反查为 mDNS 串号
    fn ip_to_mdns_serial(&self, ip_port: &str) -> Option<String> {
        self.mdns
            .ip_to_instance(ip_port)
            .map(|instance| format!("{instance}.{}", crate::mdns::CONNECT_TYPE))
    }

    // ---------- Profile / 设备状态 ----------

    /// 选中设备后的初始化：迁移旧参数、同步 profile 列表、加载应用列表
    fn init_device_state(&mut self, serial: &str) {
        // 重新配对后 mDNS 串号后缀会变：先按物理串号复制旧 profile，
        // 避免设备重连后"显示新的空白 profile"
        self.ensure_profiles_for_serial(serial);
        // 首次出现该设备：把旧版 last_app/last_resolution 等迁移进默认 profile
        if self.config.migrate_legacy_into_default_profile(serial) {
            self.config.save();
        }
        self.sync_profiles(serial);
        self.load_packages();
    }

    /// 重新配对后 mDNS 串号后缀会变（adb-<物理串号>-<随机后缀>），新串号下没有
    /// profile 时按物理串号匹配并复制旧串号的 profiles，避免"显示新的空白配置"。
    fn ensure_profiles_for_serial(&mut self, serial: &str) {
        if self.config.profiles.contains_key(serial) {
            return;
        }
        let phys = physical_serial(serial);
        let old = self
            .config
            .profiles
            .iter()
            .filter(|(k, v)| *k != serial && physical_serial(k) == phys && !v.is_empty())
            .map(|(k, _)| k.clone())
            .next();
        if let Some(old) = old {
            if self.config.copy_profiles(&old, serial) {
                self.config.save();
            }
        }
    }

    /// 从 config 同步当前设备的 profile 列表与编辑副本
    fn sync_profiles(&mut self, serial: &str) {
        self.profiles = self.config.profiles_for(serial).to_vec();
        let active = self
            .config
            .active_profile_for(serial)
            .cloned()
            .unwrap_or_else(Profile::default);
        self.profile_edit = Some(active);
        // 同步输入框（编辑副本 -> 输入框）
        // app_input 现在是下拉菜单内的过滤词，切换 profile 时清空
        self.app_input.clear();
        self.app_label_input = self
            .profile_edit
            .as_ref()
            .map(|p| p.app_label.clone())
            .unwrap_or_default();
        self.resolution_input = self
            .profile_edit
            .as_ref()
            .map(|p| p.resolution.clone())
            .unwrap_or_default();
    }

    /// 把当前编辑副本写回 config（按名字定位），并保存
    fn save_profile_edit(&mut self, serial: &str) {
        let Some(edit) = self.profile_edit.clone() else { return };
        let Some(list) = self.config.profiles_for_mut(serial) else { return };
        if let Some(p) = list.iter_mut().find(|p| p.name == edit.name) {
            *p = edit.clone();
        } else {
            // 找不到（名字被删/改名）→ 追加
            list.push(edit.clone());
        }
        self.config.active_profile = Some((serial.to_string(), edit.name.clone()));
        self.config.save();
    }

    /// 新增一个 profile 并选中
    fn add_profile(&mut self, serial: &str) {
        let n = self.profiles.len() + 1;
        let name = format!("配置 {n}");
        let p = Profile {
            name: name.clone(),
            ..Profile::default()
        };
        let list = self.config.profiles.entry(serial.to_string()).or_default();
        list.push(p);
        self.config.active_profile = Some((serial.to_string(), name));
        self.config.save();
        self.sync_profiles(serial);
    }

    /// 删除当前选中的 profile（按名字）
    fn delete_profile(&mut self, serial: &str, name: &str) {
        if let Some(list) = self.config.profiles_for_mut(serial) {
            list.retain(|p| p.name != name);
        }
        // 单独更新 active_profile（避免同时可变借用）
        let fallback = self
            .config
            .profiles_for(serial)
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        if let Some(active) = &mut self.config.active_profile {
            if active.1 == name {
                *active = (serial.to_string(), fallback);
            }
        }
        self.config.save();
        self.sync_profiles(serial);
    }

    /// 中栏 profile 改名（直接编辑）：同步 config + 本地列表 + 编辑副本
    fn rename_profile(&mut self, serial: &str, old: &str, new: &str) {
        let new = new.trim().to_string();
        if new.is_empty() || new == old {
            // 空名或未变化：还原编辑框
            self.sync_profiles(serial);
            return;
        }
        if let Some(list) = self.config.profiles_for_mut(serial) {
            if let Some(p) = list.iter_mut().find(|p| p.name == old) {
                p.name = new.clone();
            }
            if let Some(active) = &mut self.config.active_profile {
                if active.1 == old {
                    active.1 = new.clone();
                }
            }
            self.config.save();
        }
        if let Some(edit) = &mut self.profile_edit {
            if edit.name == old {
                edit.name = new.clone();
            }
        }
        self.sync_profiles(serial);
    }

    // ---------- 应用列表 ----------

    fn load_packages(&mut self) {
        let Some(serial) = self.selected_serial.clone() else { return };
        let Some(adb_path) = self.adb_path.clone() else { return };
        let scrcpy_dir = self.scrcpy_dir.clone();
        self.packages_loading = true;
        self.apps.clear();
        self.clone_pkgs.clear();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let adb = Adb::new(adb_path);
            // 优先用 scrcpy --list-apps（一次返回包名+手机端显示名）
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
            // 显示名复用主用户(user 0)里同包名的名字。
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
        // 同一手机同时出现 mDNS 串号 + ip:port 时，只保留串号条目
        let duplicate_ips: HashSet<String> = self
            .devices
            .iter()
            .filter(|d| is_ip_serial(&d.serial))
            .filter_map(|d| {
                let twin = self.ip_to_mdns_serial(&d.serial)?;
                self.devices
                    .iter()
                    .any(|x| x.serial == twin)
                    .then_some(d.serial.clone())
            })
            .collect();
        if let Some(sel) = &self.selected_serial {
            if duplicate_ips.contains(sel) {
                if let Some(twin) = self.ip_to_mdns_serial(sel) {
                    self.selected_serial = Some(twin.clone());
                    self.config.last_serial = Some(twin);
                    self.config.save();
                }
            }
        }
        if let Some(sel) = &self.selected_serial {
            if !self.devices.iter().any(|d| &d.serial == sel) {
                self.selected_serial = None;
            }
        }
        // 首次出现设备且当前无选中 → 自动选中第一个可用设备
        if self.selected_serial.is_none() {
            let first = self
                .devices
                .iter()
                .find(|d| d.state == "device")
                .map(|d| d.serial.clone());
            if let Some(s) = first {
                self.selected_serial = Some(s.clone());
                self.config.last_serial = Some(s.clone());
                self.config.save();
                self.init_device_state(&s);
            }
        }
        // 刷新已知设备分辨率（仅缓存缺失的）
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

    /// 设备分区：主列表显示，还是折叠到"已过滤"区。
    /// IP 格式设备（含 127.0.0.1 模拟器、与 mDNS 串号孪生的 IP 条目）统一由
    /// 「屏蔽 IP 格式设备」复选框控制：勾选 → 折叠区；取消勾选 → 全部显示。
    /// emulator- 前缀（AVD 模拟器别名）始终折叠。
    fn is_filtered_device(&self, serial: &str) -> bool {
        if serial.starts_with("emulator-") {
            return true;
        }
        if is_ip_serial(serial) {
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
            self.mdns
                .services(crate::mdns::CONNECT_TYPE)
                .into_iter()
                .find(|s| format!("{}.{}", s.instance, s.service) == serial)
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
            self.action_status = "未找到 adb，请先安装 scrcpy".into();
            self.action_error = true;
            return;
        };
        self.pairing_cancel.store(true, Ordering::Relaxed);
        self.pairing_cancel = Arc::new(AtomicBool::new(false));
        self.qr_expired = false;
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
                self.action_status = format!("生成二维码失败: {e}");
                self.action_error = true;
                return;
            }
        }
        self.qr = Some(qr.clone());
        self.pairing_status = "等待手机扫码配对…（2 分钟内有效）".into();

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

    /// 取当前编辑中的 profile（无则默认空 profile）
    fn current_profile(&self) -> Profile {
        self.profile_edit.clone().unwrap_or_default()
    }

    /// 「启动 app」：按当前 profile 启动应用（机主/分身，虚拟显示器/直接镜像）
    fn launch_profile_app(&mut self) {
        let Some(serial) = self.selected_serial.clone() else {
            self.action_status = "请先选择设备".into();
            self.action_error = true;
            return;
        };
        let profile = self.current_profile();
        let pkg = profile.app.trim().to_string();
        if pkg.is_empty() {
            self.action_status = "请先在「应用」中选择要启动的 app".into();
            self.action_error = true;
            return;
        }
        // 手势热区 Auto 模式：投屏**结束后**自动执行物理化修复（启动前修复会被
        // 随后创建的非物理分辨率虚拟显示器再次污染，故放在 do_launch 中登记、
        // 由 scrcpy 进程退出事件触发）。
        self.do_launch(&serial, &profile, Some(&pkg));
    }

    /// 「映射屏幕」：不启动 app、不建虚拟显示器，直接镜像设备物理屏幕。
    /// 窗口尺寸留空 = 自动匹配画面。
    fn launch_mirror_screen(&mut self) {
        let Some(serial) = self.selected_serial.clone() else {
            self.action_status = "请先选择设备".into();
            self.action_error = true;
            return;
        };
        let profile = self.current_profile();
        let Some(scrcpy) = self.scrcpy() else {
            self.action_status = "未找到 scrcpy，请先安装/更新 scrcpy".into();
            self.action_error = true;
            return;
        };
        let title = format!(
            "屏幕镜像 - {}",
            self.device_display(&serial)
        );
        // 直接镜像物理屏幕：分辨率/窗口全部留空，不建虚拟显示器
        let args = scrcpy.build_args(&serial, None, "", 0, 0, &title);
        self.run_scrcpy(&scrcpy, &args, false, false);
        self.persist_usage(&profile);
    }

    fn do_launch(&mut self, serial: &str, profile: &Profile, app: Option<&str>) {
        let Some(scrcpy) = self.scrcpy() else {
            self.action_status = "未找到 scrcpy，请先安装/更新 scrcpy".into();
            self.action_error = true;
            return;
        };
        let res = profile.resolution.trim().to_string();
        if !res.is_empty() && !valid_resolution(&res) {
            self.action_status = "分辨率格式应为 宽x高，例如 1920x1080（留空 = 直接镜像物理屏幕）".into();
            self.action_error = true;
            return;
        }
        // 窗口默认与分辨率相同：未填窗口尺寸时取分辨率的宽高
        let ww: u32 = self.win_w_input.trim().parse().unwrap_or(0);
        let wh: u32 = self.win_h_input.trim().parse().unwrap_or(0);
        let (ww, wh) = if ww == 0 && wh == 0 && !res.is_empty() {
            res.split_once('x')
                .and_then(|(a, b)| a.parse::<u32>().ok().zip(b.parse::<u32>().ok()))
                .unwrap_or((0, 0))
        } else {
            (ww, wh)
        };
        let label = profile.app_label.trim().to_string();
        let pkg = app.unwrap_or("").to_string();
        let title = format!(
            "{} - {}",
            if label.is_empty() {
                if pkg.is_empty() { "屏幕镜像".to_string() } else { pkg.clone() }
            } else {
                label
            },
            self.device_display(serial)
        );
        // 诊断：设备物理分辨率 vs 请求分辨率 vs 窗口比例
        let phys = self.device_sizes.get(serial).copied();
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
        self.action_status = diag.clone();
        self.action_error = false;

        // 分身场景（profile 记录了分身用户）
        let clone_user = profile.clone_user;
        if let Some(uid) = clone_user {
            if app.is_none() {
                // 映射屏幕 + 分身 profile：镜像主屏即可（分身信息仅用于启动 app）
                let args = scrcpy.build_args(serial, None, &res, ww, wh, &title);
                self.run_scrcpy(&scrcpy, &args, false, false);
                self.persist_usage(profile);
                return;
            }
            let component = match self.adb() {
                Some(adb) => match adb.resolve_activity(serial, uid, &pkg) {
                    Ok(c) => c,
                    Err(e) => {
                        let msg = format!("分身(user {uid})解析启动 Activity 失败: {e}");
                        self.action_status = msg.clone();
                        self.action_error = true;
                        return;
                    }
                },
                None => {
                    self.action_status = "未找到 adb".into();
                    self.action_error = true;
                    return;
                }
            };
            self.last_resolved_component = Some((uid, pkg.clone(), component.clone()));

            // 分身固定使用虚拟显示器模式：分身显示在 scrcpy 窗口、手机屏幕不被占用
            // （投屏期间系统手势不可用，结束后由后台线程自动物理化修复）
            {
                // 分身虚拟显示器必须有分辨率：显式设置优先；为空时回退设备物理
                // 分辨率（物理尺寸 VD 下手机本机手势也不受污染、scrcpy 触控正常）
                let res = if res.is_empty() {
                    let phys = self
                        .device_sizes
                        .get(serial)
                        .copied()
                        .or_else(|| self.adb().and_then(|a| a.wm_size(serial)));
                    match phys {
                        Some((w, h)) => {
                            let r = format!("{w}x{h}");
                            self.action_status = format!(
                                "分身模式需虚拟显示器，分辨率留空已自动使用设备物理分辨率 {r}"
                            );
                            self.action_error = false;
                            r
                        }
                        None => {
                            self.action_status =
                                "分身模式需要分辨率：请在上方「分辨率」填写，或确认设备已连接（留空无法创建虚拟显示器）".into();
                            self.action_error = true;
                            return;
                        }
                    }
                } else {
                    res
                };
                let args = scrcpy.build_clone_args(serial, &res, ww, wh, &title);
                let (display_id, _child) = match scrcpy.launch_with_new_display_child(&args) {
                    Ok(v) => v,
                    Err(e) => {
                        let msg = format!("创建虚拟显示器失败（未启动分身应用）: {e}");
                        self.action_status = msg.clone();
                        self.action_error = true;
                        return;
                    }
                };
                match self.adb() {
                    Some(adb) => match adb.start_app_for_user_on_display(
                        serial,
                        uid,
                        &component,
                        display_id,
                    ) {
                        Ok(o) => {
                            self.action_status = format!(
                                "已把分身(user {uid})投屏到虚拟显示器 id={display_id}（手机屏幕不受影响）。系统手势不可用（Android 平台限制）：鼠标右键=返回，Alt/Super+H=桌面，Alt/Super+S=最近任务。{o}"
                            );
                            self.action_error = false;
                        }
                        Err(e) => {
                            self.action_status = format!("分身投屏启动失败: {e}");
                            self.action_error = true;
                            return;
                        }
                    },
                    None => {
                        self.action_status = "未找到 adb".into();
                        self.action_error = true;
                        return;
                    }
                }
                // 用户验证过的方案：分身投屏建立后，立刻创建一次物理尺寸虚拟显示器
                // 再移除，把主屏手势热区固化为物理尺寸——投屏期间与结束后手机本机
                // 手势都正常（无需等投屏结束再修）
                if profile.gesture_fix != GestureFixMode::Off {
                    self.repair_gesture_after_vd(serial);
                }
                self.persist_usage(profile);
            }
        } else if let Some(pkg) = app {
            // 机主应用
            let args = scrcpy.build_args(serial, Some(pkg), &res, ww, wh, &title);
            let virtual_display = args.iter().any(|a| a.contains("--new-display"));
            self.run_scrcpy(
                &scrcpy,
                &args,
                virtual_display,
                profile.gesture_fix != GestureFixMode::Off,
            );
            self.persist_usage(profile);
        } else {
            // 映射屏幕（无分身、无 app）
            let args = scrcpy.build_args(serial, None, &res, ww, wh, &title);
            let virtual_display = args.iter().any(|a| a.contains("--new-display"));
            self.run_scrcpy(
                &scrcpy,
                &args,
                virtual_display,
                profile.gesture_fix != GestureFixMode::Off,
            );
            self.persist_usage(profile);
        }
    }

    /// 投屏建立后立即执行的物理化手势修复（用户验证过的方案）：
    /// 创建一次物理尺寸虚拟显示器再移除，把主屏手势热区固化为物理尺寸，
    /// 投屏期间与结束后手机本机手势都正常。非荣耀设备自动跳过。
    fn repair_gesture_after_vd(&mut self, serial: &str) {
        let Some(adb) = self.adb() else {
            self.action_status = "未找到 adb，无法物理化修复手势".into();
            self.action_error = true;
            return;
        };
        if !adb.is_honor_device(serial) {
            return;
        }
        let Some((w, h)) = adb.wm_size(serial) else {
            self.action_status = "无法获取设备物理分辨率，手势热区未能物理化修复".into();
            self.action_error = true;
            return;
        };
        let Some(scrcpy) = self.scrcpy() else {
            self.action_status = "未找到 scrcpy，手势热区未能物理化修复".into();
            self.action_error = true;
            return;
        };
        self.action_status = format!("正在物理化修复手势热区（物理分辨率 {w}x{h} 覆盖一次）...");
        self.action_error = false;
        match scrcpy.repair_gesture_hotzone(serial, w, h) {
            Ok(()) => {
                self.action_status =
                    "投屏已建立，手机手势热区已固化为物理尺寸，本机手势正常（无需重启手机）".into();
                self.action_error = false;
            }
            Err(e) => {
                self.action_status =
                    format!("手势热区物理化修复失败: {e}（投屏结束或重启手机后可恢复）");
                self.action_error = true;
            }
        }
    }

    fn run_scrcpy(
        &mut self,
        scrcpy: &Scrcpy,
        args: &[String],
        virtual_display: bool,
        repair_now: bool,
    ) {
        self.action_status = if virtual_display {
            "scrcpy 已启动（虚拟显示器模式，手机屏幕不受影响）。系统手势不可用（Android 平台限制）：鼠标右键=返回，Alt/Super+H=桌面，Alt/Super+S=最近任务".into()
        } else {
            "scrcpy 已启动（直接镜像模式，系统手势可用）".into()
        };
        self.action_error = false;
        match scrcpy.launch(args) {
            Ok(_child) => {
                // 虚拟显示器模式：投屏建立后立即物理化修复手势热区
                // （用户验证方案：物理尺寸 VD 创建再移除 → 热区固化，手机手势恢复）
                if virtual_display && repair_now {
                    if let Some(serial) = self.selected_serial.clone() {
                        self.repair_gesture_after_vd(&serial);
                    }
                }
            }
            Err(e) => {
                self.action_status = format!("启动失败: {e}");
                self.action_error = true;
            }
        }
    }

    fn persist_usage(&mut self, profile: &Profile) {
        let Some(serial) = self.selected_serial.clone() else { return };
        // 持久化：profile 编辑内容 + 应用/分辨率历史 + 全局窗口尺寸
        self.save_profile_edit(&serial);
        if !profile.app.is_empty() {
            Config::push_unique(&mut self.config.app_history, profile.app.clone(), 20);
            if !profile.app_label.is_empty() {
                self.config
                    .app_labels
                    .insert(profile.app.clone(), profile.app_label.clone());
            }
        }
        Config::push_unique(&mut self.config.resolutions, profile.resolution.clone(), 10);
        self.config.last_app = if profile.app.is_empty() {
            None
        } else {
            Some(profile.app.clone())
        };
        self.config.last_resolution = if profile.resolution.is_empty() {
            None
        } else {
            Some(profile.resolution.clone())
        };
        self.config.last_app_label = if profile.app_label.is_empty() {
            None
        } else {
            Some(profile.app_label.clone())
        };
        self.config.window_width = self.win_w_input.trim().parse().unwrap_or(0);
        self.config.window_height = self.win_h_input.trim().parse().unwrap_or(0);
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
                Msg::DevicesTick => self.refresh_once(),
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
                    }
                }
                Msg::ScrcpyVersion(v) => self.scrcpy_version = v,
                Msg::DeviceSize(serial, size) => {
                    if let Some(sz) = size {
                        self.device_sizes.insert(serial.clone(), sz);
                    }
                }
                Msg::PairDone(r) => {
                    self.pairing_cancel = Arc::new(AtomicBool::new(true));
                    match r {
                        Ok(s) => {
                            self.pairing_status = s.clone();
                            self.refresh_once();
                        }
                        Err(e) => {
                            self.pairing_status = e.clone();
                            // 配对超时 → 二维码标记失效（变灰、点击重新生成）
                            if e.contains("超时") {
                                self.qr_expired = true;
                            }
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
                        }
                        Err(e) => {
                            self.update_status = format!("更新失败: {e}");
                        }
                    }
                }
                Msg::Log(_s) => {}
            }
        }
    }

    // ---------- UI ----------

    /// 左栏：设备列表 + 选中设备操作 + scrcpy 更新区 + 二维码配对区
    fn ui_left(&mut self, ui: &mut egui::Ui) {
        ui.heading("设备");
        if self.adb_path.is_none() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 120, 120),
                "未找到 adb / scrcpy",
            );
            if ui.button("下载并安装 scrcpy（含 adb）").clicked() {
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
            .checkbox(&mut self.config.hide_ip_devices, "屏蔽 IP 格式设备（同手机只显示串号）")
            .on_hover_text("同一手机在 adb 里会同时出现 mDNS 串号与 ip:port 两个条目，默认只显示串号；没有对应串号的纯 IP 设备放进下方折叠区。取消勾选后全部显示。")
            .changed()
        {
            self.config.save();
            self.refresh_once();
        }

        let mut to_select: Option<String> = None;
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
                self.config.last_serial = Some(serial.clone());
                self.config.save();
                self.init_device_state(&serial);
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
                if ui.button("复制串号").clicked() {
                    self.copy_text(&serial);
                }
            });
            ui.horizontal(|ui| {
                ui.label("重命名:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.rename_input).hint_text("输入设备别名"),
                );
                if ui.button("保存").clicked() {
                    self.save_rename(&serial);
                }
            });
        }

        // 二维码配对区（收窄：只二维码 + 重新生成；说明/密码进"？"帮助）
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("无线调试配对");
            let help = "二维码：与手机「开发者选项 → 无线调试 → 使用二维码配对设备」配合使用。\n\
                扫码后手机广播自己的配对服务，PC 会自动 adb pair 并连接，2 分钟内有效。\n\
                若二维码失效/失败，可用下方「配对码方式」手动配对（手机页面会显示 ip:port 与 6 位配对码）。";
            if ui
                .add(egui::Button::new("？").corner_radius(8.0))
                .on_hover_text(help)
                .clicked()
            {
                // 点击 "？" 展开帮助
                self.qr_help_open = !self.qr_help_open;
            }
        });
        if self.qr_help_open {
            ui.label(egui::RichText::new("二维码：与手机「开发者选项 → 无线调试 → 使用二维码配对设备」配合使用。扫码后手机广播配对服务，PC 自动 adb pair 并连接，2 分钟内有效。若失败可用配对码方式手动配对。").small().weak());
            if !self.pairing_status.is_empty() {
                ui.colored_label(
                    if self.qr_expired {
                        egui::Color32::from_rgb(255, 170, 90)
                    } else {
                        egui::Color32::from_rgb(150, 190, 255)
                    },
                    &self.pairing_status,
                );
            }
        }
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
                if let Some(qr) = &self.qr {
                    // 密码进 "？" 帮助
                    ui.label(
                        egui::RichText::new(format!("配对密码 {}", qr.password))
                            .small()
                            .weak(),
                    )
                    .on_hover_text("此密码仅用于手动配对，扫码时无需输入");
                }
            });
            if let Some(tex) = &self.qr_texture {
                let size = egui::vec2(180.0, 180.0);
                if self.qr_expired {
                    // 失效效果：二维码整体变灰 + 半透明遮罩 + 中央「点击刷新」，
                    // 点击二维码即重新生成（同支付二维码失效样式）
                    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                    let painter = ui.painter();
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    painter.image(tex.id(), rect, uv, egui::Color32::from_gray(70));
                    painter.rect_filled(
                        rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(15, 15, 20, 160),
                    );
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "二维码已失效\n点击刷新",
                        egui::FontId::proportional(14.0),
                        egui::Color32::WHITE,
                    );
                    if resp.clicked() {
                        self.qr = None;
                        self.qr_texture = None;
                        self.qr_expired = false;
                        self.pairing_status.clear();
                        let ctx = ui.ctx().clone();
                        self.start_pairing(&ctx);
                    }
                } else {
                    ui.add(egui::Image::new((tex.id(), size)));
                }
            }
        }

        ui.separator();
        ui.label("配对码方式（二维码失败时用）:");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.pair_code_ip)
                    .hint_text("手机无线调试页 ip:port")
                    .desired_width(130.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.pair_code)
                    .hint_text("6 位配对码")
                    .desired_width(80.0),
            );
        });
        if ui.button("配对并连接").clicked() {
            self.action_pair_with_code();
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("手动连接:");
            ui.add(
                egui::TextEdit::singleline(&mut self.manual_ip).hint_text("如 192.168.1.5:37855"),
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

        // scrcpy 更新区（连接下方）
        ui.separator();
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

    }

    /// 中栏：当前设备的 profile 列表（增/删/改/直接改名）
    fn ui_profiles(&mut self, ui: &mut egui::Ui) {
        let Some(serial) = self.selected_serial.clone() else {
            ui.heading("配置（Profile）");
            ui.label("请先在左侧选择设备");
            return;
        };
        ui.heading("配置（Profile）");
        ui.label(egui::RichText::new(format!("设备: {}", self.device_display(&serial))).small());
        if ui.button("＋ 新建配置").clicked() {
            self.add_profile(&serial);
        }
        ui.separator();
        let active_name = self
            .config
            .active_profile
            .as_ref()
            .filter(|(s, _)| s == &serial)
            .map(|(_, n)| n.clone());
        let mut to_rename: Option<(String, String)> = None;
        let mut to_delete: Option<String> = None;
        let mut to_select: Option<String> = None;
        // 重命名输入区（常驻控件：输入框每帧渲染、文本存字段，避免临时控件输入不同步；
        // 点击「改名」后自动聚焦，直接输入新名字）
        if let Some(rename_target) = self.renaming_profile.clone() {
            ui.horizontal(|ui| {
                ui.label("重命名:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.renaming_name)
                        .desired_width(150.0),
                );
                if self.rename_focus_requested {
                    resp.request_focus();
                    self.rename_focus_requested = false;
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let new_name = self.renaming_name.trim().to_string();
                    if !new_name.is_empty() {
                        to_rename = Some((rename_target.clone(), new_name));
                    }
                    self.renaming_profile = None;
                }
                if ui
                    .button(egui::RichText::new("✔").small())
                    .on_hover_text("保存新名字")
                    .clicked()
                {
                    let new_name = self.renaming_name.trim().to_string();
                    if !new_name.is_empty() {
                        to_rename = Some((rename_target.clone(), new_name));
                    }
                    self.renaming_profile = None;
                }
                if ui
                    .button(egui::RichText::new("×").small())
                    .on_hover_text("取消改名")
                    .clicked()
                {
                    self.renaming_profile = None;
                }
            });
            ui.separator();
        }
        let names: Vec<String> = self.profiles.iter().map(|p| p.name.clone()).collect();
        for name in names {
            let is_active = active_name.as_deref() == Some(name.as_str());
            ui.horizontal(|ui| {
                // 只读态：点名字=选中该配置；点「改名」进入顶部常驻重命名输入区
                if ui
                    .selectable_label(is_active, &name)
                    .on_hover_text("点击选中此配置")
                    .clicked()
                {
                    to_select = Some(name.clone());
                }
                let action = ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let mut a = None;
                        if ui
                            .button(egui::RichText::new("×").small())
                            .on_hover_text("删除此配置")
                            .clicked()
                        {
                            a = Some(0);
                        }
                        if ui
                            .button(egui::RichText::new("改名").small())
                            .on_hover_text("重命名此配置")
                            .clicked()
                        {
                            a = Some(1);
                        }
                        a
                    },
                );
                match action.inner {
                    Some(1) => {
                        self.renaming_profile = Some(name.clone());
                        self.renaming_name = name.clone();
                        self.rename_focus_requested = true;
                    }
                    Some(0) => to_delete = Some(name.clone()),
                    _ => {}
                }
            });
        }
        if let Some((old, new)) = to_rename {
            self.rename_profile(&serial, &old, &new);
        }
        if let Some(name) = to_delete {
            self.delete_profile(&serial, &name);
        }
        if let Some(name) = to_select {
            self.config.active_profile = Some((serial.clone(), name.clone()));
            self.config.save();
            self.sync_profiles(&serial);
        }
        if self.profiles.is_empty() {
            ui.label(egui::RichText::new("（暂无配置，点「＋ 新建配置」创建）").weak().small());
        }
    }

    /// 右栏：profile 编辑器
    fn ui_profile_editor(&mut self, ui: &mut egui::Ui) {
        let Some(serial) = self.selected_serial.clone() else {
            ui.heading("启动");
            ui.label("请先在左侧选择设备");
            return;
        };
        let Some(mut edit) = self.profile_edit.clone() else {
            ui.heading("启动");
            ui.label("请先在中栏选择或新建配置");
            return;
        };
        ui.heading("启动");
        ui.label(egui::RichText::new(format!("配置: {}", edit.name)).small().weak());

        // 两个启动按钮
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let btn1 = ui.add_enabled(
                !edit.app.trim().is_empty(),
                egui::Button::new(
                    egui::RichText::new("启动 app").size(16.0).strong(),
                )
                .min_size(egui::vec2(110.0, 34.0)),
            );
            if btn1.on_hover_text("按此配置启动应用（分身/虚拟显示器逻辑按配置执行）").clicked() {
                self.launch_profile_app();
            }
            let btn2 = ui.add(
                egui::Button::new(
                    egui::RichText::new("映射屏幕").size(16.0).strong(),
                )
                .min_size(egui::vec2(110.0, 34.0)),
            );
            if btn2.on_hover_text("不启动应用，直接投屏当前设备（分辨率按本配置，留空=直接镜像）").clicked() {
                self.launch_mirror_screen();
            }
        });
        ui.add_space(4.0);
        ui.separator();

        // 应用选择：egui 标准下拉框（ComboBox，自绘箭头/弹层，无字体缺失问题）
        // selected_text 显示应用名字（与选项一致）；菜单内顶部可输入过滤，机主/分身分区
        ui.label("应用:");
        let mut edit_app = edit.app.clone();
        let mut edit_label = edit.app_label.clone();
        let mut edit_clone = edit.clone_user;
        let mut apply_from_list: Option<(String, Option<i32>, String)> = None;
        let selected_text = if edit.app.is_empty() {
            "选择应用…".to_string()
        } else {
            // 显示「名字 包名」；分身追加（分身）标记
            let base = if edit.app_label.is_empty() {
                edit.app.clone()
            } else {
                format!("{}  {}", edit.app_label, edit.app)
            };
            if edit.clone_user.is_some() {
                format!("{}（分身）", base)
            } else {
                base
            }
        };
        // 应用下拉：自建标准下拉框（Button 显示选中项 + 右侧三角，点击弹层）
        // 弹层内顶部过滤输入框（输入即过滤，点击不会关闭弹层）+ 机主/分身分区；
        // 点击弹层外部或选中选项自动关闭
        let popup_id = ui.make_persistent_id("app_picker_area");
        let resp = ui.add(
            egui::Button::new(egui::RichText::new(selected_text).monospace())
                .min_size(egui::vec2(300.0, 24.0)),
        );
        // 右侧自绘下拉三角（无字形缺失问题）
        if ui.is_rect_visible(resp.rect) {
            let painter = ui.painter_at(resp.rect);
            let c = egui::pos2(resp.rect.right() - 10.0, resp.rect.center().y);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x - 3.5, c.y - 1.5),
                    egui::pos2(c.x + 3.5, c.y - 1.5),
                    egui::pos2(c.x, c.y + 2.5),
                ],
                ui.visuals().text_color(),
                egui::Stroke::NONE,
            ));
        }
        if resp.clicked() {
            self.app_combo_open = !self.app_combo_open;
        }
        let mut picked_out: Option<(String, Option<i32>, String)> = None;
        if self.app_combo_open {
            let area = egui::Area::new(popup_id)
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(resp.rect.left(), resp.rect.bottom() + 2.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(300.0);
                        // 菜单内过滤输入框（输入即过滤，点击不会关闭弹层）
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("过滤:").small());
                            ui.add(
                                egui::TextEdit::singleline(&mut self.app_input)
                                    .hint_text("输入包名或应用名过滤")
                                    .desired_width(230.0),
                            );
                        });
                        ui.separator();
                        let filter = self.app_input.to_lowercase();
                        let picked = egui::ScrollArea::vertical()
                            .id_salt("app_picker_scroll")
                            .max_height(360.0)
                            .show(ui, |ui| {
                        let mut picked = None;
                        // 机主应用（独立计数，避免顶掉分身分区）
                        let mut shown_main = 0;
                        for app in self.apps.iter().filter(|a| {
                            filter.is_empty()
                                || a.package.to_lowercase().contains(&filter)
                                || a.name.to_lowercase().contains(&filter)
                        }) {
                            if shown_main >= 60 {
                                ui.label(
                                    egui::RichText::new("…（继续输入过滤词缩小范围）")
                                        .weak()
                                        .small(),
                                );
                                break;
                            }
                            let text = if app.name.is_empty() {
                                app.package.clone()
                            } else {
                                format!("{}  {}", app.name, app.package)
                            };
                            let is_sel = edit_app == app.package && edit_clone.is_none();
                            if ui.selectable_label(is_sel, text).clicked() {
                                picked = Some((app.package.clone(), None, app.name.clone()));
                            }
                            shown_main += 1;
                        }
                        // 分身分区（独立计数）
                        let mut shown_clone = 0;
                        let mut has_clone = false;
                        for (uid, uname, app) in self.clone_pkgs.iter().filter(|(_, _, a)| {
                            filter.is_empty()
                                || a.package.to_lowercase().contains(&filter)
                                || a.name.to_lowercase().contains(&filter)
                        }) {
                            if !has_clone {
                                ui.separator();
                                ui.label(egui::RichText::new("— 分身应用 —").weak().small());
                                has_clone = true;
                            }
                            if shown_clone >= 60 {
                                ui.label(
                                    egui::RichText::new("…（分身列表过多，继续输入过滤）")
                                        .weak()
                                        .small(),
                                );
                                break;
                            }
                            // 分身实例名已含「分身」时不再加前缀（[分身 分身应用] -> [分身应用]）
                            let clone_tag = if uname.contains("分身") {
                                format!("[{uname}]")
                            } else {
                                format!("[分身 {uname}]")
                            };
                            let text = if app.name.is_empty() {
                                format!("{}  {}", app.package, clone_tag)
                            } else {
                                format!("{}  {}  {}", app.name, app.package, clone_tag)
                            };
                            let is_sel = edit_app == app.package && edit_clone == Some(*uid);
                            if ui.selectable_label(is_sel, text).clicked() {
                                picked =
                                    Some((app.package.clone(), Some(*uid), app.name.clone()));
                            }
                            shown_clone += 1;
                        }
                        if self.apps.is_empty()
                            && self.clone_pkgs.is_empty()
                            && !self.packages_loading
                        {
                            ui.label(
                                egui::RichText::new(
                                    "应用列表为空（加载失败或设备未连接），点下方「刷新应用列表」重试",
                                )
                                .weak()
                                .small(),
                            );
                        }
                        picked
                            })
                            .inner;
                        (picked, ui.min_rect())
                    })
                    .inner
                });
            // 点击弹层外部关闭：以「按下位置」是否落在弹层内容矩形内判定，
            // 点过滤框/列表（弹层内）不会关闭；排除打开当帧的按钮点击（否则一打开就被关掉）
            if ui.input(|i| i.pointer.any_pressed()) {
                let origin = ui.input(|i| i.pointer.press_origin());
                let inside = origin.map_or(false, |p| area.inner.1.contains(p));
                if !inside && !resp.clicked() {
                    self.app_combo_open = false;
                }
            }
            picked_out = area.inner.0;
        }
        if let Some((pkg, uid, name)) = picked_out {
            apply_from_list = Some((pkg, uid, name));
            self.app_combo_open = false;
        }
        if let Some((pkg, uid, name)) = apply_from_list {
            edit_app = pkg;
            edit_clone = uid;
            // 选中新应用时默认填应用显示名（可修改）
            edit_label = name;
            // 清空过滤词，下次打开菜单显示完整列表
            self.app_input.clear();
        }
        if self.packages_loading {
            ui.spinner();
        }
        if ui.button("刷新应用列表").clicked() {
            self.load_packages();
        }

        ui.separator();
        ui.label("窗口标题（默认应用名称，可修改）:");
        ui.add(
            egui::TextEdit::singleline(&mut edit_label)
                .hint_text("如 无尽冬日")
                .desired_width(300.0),
        );

        ui.separator();
        ui.label("分辨率（留空 = 直接镜像物理屏幕）:");
        let mut edit_res = edit.resolution.clone();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("res_history")
                .selected_text(if edit_res.is_empty() {
                    "留空 = 直接镜像物理屏幕".to_string()
                } else {
                    edit_res.clone()
                })
                .width(170.0)
                .show_ui(ui, |ui| {
                    // 历史分辨率列表（留空 = 直接镜像，通过清空输入框实现）
                    for item in self.config.resolutions.clone() {
                        if ui
                            .selectable_label(edit_res == item, &item)
                            .clicked()
                        {
                            edit_res = item;
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut edit_res)
                    .hint_text("留空=直接镜像")
                    .desired_width(100.0),
            );
        });
        ui.label("窗口尺寸（留空 = 与分辨率相同）:");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.win_w_input)
                    .desired_width(60.0)
                    .hint_text("宽"),
            );
            ui.label("x");
            ui.add(
                egui::TextEdit::singleline(&mut self.win_h_input)
                    .desired_width(60.0)
                    .hint_text("高"),
            );
        });

        // 应用/标题/分辨率变更 → 写回编辑副本并保存（分身投屏固定为虚拟显示器模式，
        // 手势热区固定为 Auto，投屏结束后由后台线程自动物理化修复，均无需界面配置）

        // 投屏结束后由后台线程自动物理化修复，不再需要界面配置）
        let mut changed = false;
        if edit_app != edit.app {
            edit.app = edit_app;
            changed = true;
        }
        if edit_clone != edit.clone_user {
            edit.clone_user = edit_clone;
            changed = true;
        }
        if edit_label != edit.app_label {
            edit.app_label = edit_label;
            changed = true;
        }
        if edit_res != edit.resolution {
            edit.resolution = edit_res;
            changed = true;
        }
        if changed {
            self.profile_edit = Some(edit.clone());
            self.save_profile_edit(&serial);
            self.sync_profiles(&serial);
        }
        if self.win_w_input.trim().parse::<u32>().is_ok()
            || self.win_h_input.trim().parse::<u32>().is_ok()
        {
            let _ = self.win_w_input.clone();
        }
    }
}

impl eframe::App for GScrcpyApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_messages();
        ctx.request_repaint_after(Duration::from_millis(300));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 底部状态栏：操作/启动提示统一显示在这里（不再占用左栏）
        egui::Panel::bottom("status_bar")
            .resizable(false)
            .default_size(26.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if !self.action_status.is_empty() {
                        let color = if self.action_error {
                            egui::Color32::from_rgb(255, 110, 110)
                        } else {
                            egui::Color32::from_rgb(150, 190, 255)
                        };
                        ui.colored_label(color, &self.action_status);
                    }
                    let selected_display = self
                        .selected_serial
                        .clone()
                        .map(|s| self.device_display(&s));
                    if let Some(d) = selected_display {
                        ui.label(egui::RichText::new(format!("· 选中 {d}")).weak().small());
                    }
                });
            });
        // 三栏：左=设备，中=Profile 列表，右=Profile 编辑
        egui::Panel::left("left_devices")
            .resizable(true)
            .default_size(290.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.ui_left(ui);
                });
            });
        egui::Panel::left("middle_profiles")
            .resizable(true)
            .default_size(230.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.ui_profiles(ui);
                });
            });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.ui_profile_editor(ui);
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

/// 是否为 IP 格式串号（如 "192.168.1.5:37855"）
/// 本机常见模拟器 adb 端口（仅探测 127.0.0.1，不触碰局域网设备）
const LOCAL_EMULATOR_PORTS: &[u16] = &[7555, 16384, 16385, 5555, 5554];

/// 上次探测模拟器端口的时间（秒），5 秒冷却避免高频刷新时反复探测
static LAST_EMU_PROBE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 探测本机常见的模拟器 adb 端口并自动 `adb connect`（MuMu 7555/16384/16385、
/// 通用 5555/5554 等）。打开模拟器后无需手动连接，设备列表刷新即可出现。
fn try_connect_local_emulators(adb: &Adb) {
    use std::sync::atomic::Ordering as AOrdering;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(LAST_EMU_PROBE.load(AOrdering::Relaxed)) < 5 {
        return;
    }
    LAST_EMU_PROBE.store(now, AOrdering::Relaxed);
    let known: std::collections::HashSet<String> = adb
        .devices()
        .into_iter()
        .map(|d| d.serial)
        .collect();
    for port in LOCAL_EMULATOR_PORTS {
        let hp = format!("127.0.0.1:{port}");
        if known.contains(&hp) {
            continue;
        }
        // 快速 TCP 探测：端口开放才 connect（closed 端口 connect 会等很久）
        let addr: std::net::SocketAddr = hp.parse().unwrap();
        let ok = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok();
        if ok {
            let _ = adb.connect(&hp);
        }
    }
}

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
    host.starts_with(|c: char| c.is_ascii_digit()) || host.starts_with('[')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_serial_predicate() {
        assert!(is_ip_serial("192.168.1.5:37855"));
        assert!(is_ip_serial("10.0.0.8:5555"));
        assert!(is_ip_serial("127.0.0.1:7555"));
        assert!(!is_ip_serial("emulator-5554"));
        assert!(!is_ip_serial("adb-D1222091020A-aWsoaY._adb-tls-connect._tcp"));
        assert!(!is_ip_serial("127.0.0.1"));
        assert!(!is_ip_serial("f51065db"));
    }
}

/// 渲染一行设备并返回点击响应
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
        let color = if dev.state == "offline" {
            egui::Color32::from_rgb(110, 145, 190)
        } else {
            egui::Color32::from_rgb(150, 195, 245)
        };
        ui.add(egui::Button::new(text.color(color)).frame(false))
    }
}

/// 加载系统中文字体作为回退字体
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
