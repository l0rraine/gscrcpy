use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::adb::{Adb, DeviceInfo, MdnsService};
use crate::config::Config;
use crate::pairing::{self, PairingQr};
use crate::scrcpy::Scrcpy;
use crate::updater;

pub enum Msg {
    Devices(Vec<DeviceInfo>),
    Packages(String, Vec<String>), // (请求时的串号, 结果)
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
    /// mDNS 服务缓存（ip:port -> mDNS 串号反查用，60s 刷新）
    mdns_map: Vec<MdnsService>,
    mdns_map_at: std::time::Instant,

    packages: Vec<String>,
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

    scrcpy_version: Option<String>,
    latest: Option<Result<updater::ReleaseInfo, String>>,
    update_status: String,
    update_working: bool,
}

impl GScrcpyApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cjk_fonts(&cc.egui_ctx);
        let config = Config::load();
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
            mdns_map: Vec::new(),
            mdns_map_at: std::time::Instant::now(),
            packages: Vec::new(),
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
            scrcpy_version: None,
            latest: None,
            update_status: String::new(),
            update_working: false,
        };
        // 初始化输入
        app.app_input = app.config.last_app.clone().unwrap_or_default();
        app.app_label_input = app.config.last_app_label.clone().unwrap_or_default();
        app.resolution_input = app
            .config
            .last_resolution
            .clone()
            .unwrap_or_else(|| "1920x1080".into());
        app.win_w_input = app.config.window_width.to_string();
        app.win_h_input = app.config.window_height.to_string();
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
        if serial.contains(':') && !serial.starts_with("127.0.0.1:") {
            if let Some(mdns) = self.mdns_lookup(serial) {
                return self.config.display_name(&mdns, model.as_deref());
            }
        }
        self.config.display_name(serial, model.as_deref())
    }

    /// 通过缓存的 mDNS 服务把 ip:port 反查为 mDNS 串号（60s 刷新一次）
    fn mdns_lookup(&mut self, ip_port: &str) -> Option<String> {
        if self.mdns_map_at.elapsed() > Duration::from_secs(60) {
            self.mdns_map = self
                .adb()
                .map(|adb| adb.mdns_services())
                .unwrap_or_default();
            self.mdns_map_at = std::time::Instant::now();
        }
        self.mdns_map
            .iter()
            .find(|s| {
                s.service == "_adb-tls-connect._tcp"
                    && format!("{}:{}", s.host, s.port) == ip_port
            })
            .map(|s| format!("{}.{}", s.instance, s.service))
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
        self.packages_loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let adb = Adb::new(adb_path);
            let pkgs = adb.packages(&serial);
            let _ = tx.send(Msg::Packages(serial, pkgs));
        });
    }

    fn apply_devices(&mut self, devs: Vec<DeviceInfo>) {
        self.devices = devs;
        // 上次选中的设备还在，则保持
        if let Some(sel) = &self.selected_serial {
            if !self.devices.iter().any(|d| &d.serial == sel) {
                self.selected_serial = None;
            }
        }
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
            // mDNS 串号 -> 解析为 ip:port
            adb.mdns_services()
                .into_iter()
                .find(|s| {
                    s.service == "_adb-tls-connect._tcp"
                        && format!("{}.{}", s.instance, s.service) == serial
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
        self.pairing_cancel = Arc::new(AtomicBool::new(false));
        self.pairing_status = "等待手机扫码配对…（2 分钟内有效）".into();

        let tx = self.tx.clone();
        let cancel = self.pairing_cancel.clone();
        std::thread::spawn(move || {
            let r = pairing::pair_loop(adb_path, &qr, &cancel);
            let _ = tx.send(Msg::PairDone(r));
        });
        self.log("已生成配对二维码。请在同一 WiFi 下，打开手机「开发者选项 → 无线调试 → 使用二维码配对设备」扫码。");
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
        if !valid_resolution(&res) {
            self.log("分辨率格式应为 宽x高，例如 1920x1080");
            return;
        }
        let ww: u32 = self.win_w_input.parse().unwrap_or(0);
        let wh: u32 = self.win_h_input.parse().unwrap_or(0);
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
        let args = scrcpy.build_args(&serial, &pkg, &res, ww, wh, &title);
        self.log(format!(
            "启动: {} {}",
            scrcpy.exe().display(),
            args.join(" ")
        ));
        match scrcpy.launch(&args) {
            Ok(()) => {
                self.action_status = "scrcpy 已启动（详细日志见 scrcpy.log）".into();
                self.action_error = false;
                self.log("scrcpy 已启动（详细日志见 scrcpy.log）");
            }
            Err(e) => {
                self.action_status = format!("启动失败: {e}");
                self.action_error = true;
                self.log(e);
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
                Msg::Packages(serial, pkgs) => {
                    if self.selected_serial.as_deref() == Some(serial.as_str()) {
                        self.packages = pkgs;
                        self.packages_loading = false;
                        self.log(format!("已加载 {} 个应用", self.packages.len()));
                    }
                }
                Msg::ScrcpyVersion(v) => self.scrcpy_version = v,
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

        let mut to_select: Option<String> = None;
        // 主列表过滤掉模拟器与本机回环连接（127.0.0.1）的设备，被过滤的单独展示在折叠区
        let (main_devs, filtered_devs): (Vec<&DeviceInfo>, Vec<&DeviceInfo>) = self
            .devices
            .iter()
            .partition(|d| !is_filtered_device(&d.serial));
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
                self.packages.clear();
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
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.app_input)
                    .hint_text("如 com.gof.china")
                    .desired_width(260.0),
            );
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
            ui.label("虚拟分辨率:");
            egui::ComboBox::from_id_salt("res_history")
                .selected_text(self.resolution_input.clone())
                .width(140.0)
                .show_ui(ui, |ui| {
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
                    .hint_text("宽x高，如 1920x1080")
                    .desired_width(140.0),
            );
            ui.label("窗口:");
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
                    if let Some(scrcpy) = self.scrcpy() {
                        let args = scrcpy.build_args(s, &pkg, &res, ww, wh, &title);
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
        let filtered: Vec<String> = self
            .packages
            .iter()
            .filter(|p| filter.is_empty() || p.to_lowercase().contains(&filter))
            .cloned()
            .collect();
        ui.label(format!("共 {} 个", filtered.len()));
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .show(ui, |ui| {
                for pkg in filtered {
                    if ui.selectable_label(false, &pkg).clicked() {
                        self.copy_text(&pkg);
                        self.app_input = pkg.clone();
                        if self.app_label_input.is_empty() {
                            self.app_label_input = pkg.clone();
                        }
                        self.log(format!("已复制并填入: {pkg}"));
                    }
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

/// 是否属于主列表隐藏的设备：模拟器（emulator- 前缀）或本机回环地址连接（127.0.0.1:端口）
fn is_filtered_device(serial: &str) -> bool {
    serial.starts_with("emulator-") || serial.starts_with("127.0.0.1:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_device_predicate() {
        assert!(is_filtered_device("emulator-5554"));
        assert!(is_filtered_device("127.0.0.1:7555"));
        assert!(is_filtered_device("127.0.0.1:16384"));
        assert!(!is_filtered_device("adb-D1222091020A-aWsoaY._adb-tls-connect._tcp"));
        assert!(!is_filtered_device("192.168.1.5:37855"));
        assert!(!is_filtered_device("127.0.0.1"));
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
