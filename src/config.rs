use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 手势热区修复模式（荣耀/华为专用，见 `Scrcpy::repair_gesture_hotzone`）
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GestureFixMode {
    /// 每次启动 profile 时自动执行一次物理化修复（覆盖为设备物理分辨率）
    Auto,
    /// 仅在用户点击「修复手势热区」时执行一次
    Manual,
    /// 不修复
    Off,
}

impl Default for GestureFixMode {
    fn default() -> Self {
        // 默认每次投屏结束自动执行物理化修复（荣耀/华为），非荣耀设备会自动跳过
        GestureFixMode::Auto
    }
}

/// 一台设备的一个启动配置（Profile）。设备与 profile 是**多对一**关系：
/// `Config::profiles[设备串号] -> Vec<Profile>`。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Profile {
    /// profile 名称（设备内唯一）
    pub name: String,
    /// 包名（空 = 仅映射屏幕，不启动 app）
    pub app: String,
    /// 分身用户（Some = 分身应用，如荣耀 user 128；None = 机主）
    #[serde(default)]
    pub clone_user: Option<i32>,
    /// 窗口标题（空 = 用应用显示名/包名）
    #[serde(default)]
    pub app_label: String,
    /// 虚拟分辨率（空 = 直接镜像物理屏幕）
    #[serde(default)]
    pub resolution: String,
    /// 手势热区修复模式
    #[serde(default)]
    pub gesture_fix: GestureFixMode,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "默认".to_string(),
            app: String::new(),
            clone_user: None,
            app_label: String::new(),
            resolution: String::new(),
            gesture_fix: GestureFixMode::Off,
        }
    }
}

/// 应用配置，持久化到 %APPDATA%/gscrcpy/config.json
#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    /// 设备串号 -> 用户自定义别名
    #[serde(default)]
    pub device_aliases: HashMap<String, String>,
    /// 最近使用的应用类名（包名）历史，最新的在前
    #[serde(default)]
    pub app_history: Vec<String>,
    /// 包名 -> 显示名（用于窗口标题，如 "com.tencent.tmgp.sgame" -> "王者荣耀"）
    #[serde(default)]
    pub app_labels: HashMap<String, String>,
    /// 最近使用的虚拟分辨率历史，最新的在前（如 "1920x1080"）
    #[serde(default)]
    pub resolutions: Vec<String>,
    /// scrcpy 窗口宽（0 表示自动）
    #[serde(default = "default_window_width")]
    pub window_width: u32,
    /// scrcpy 窗口高（0 表示自动）
    #[serde(default = "default_window_height")]
    pub window_height: u32,
    /// scrcpy 所在目录（内含 scrcpy.exe 与 adb.exe）
    #[serde(default)]
    pub scrcpy_dir: Option<PathBuf>,
    /// 上次选中的设备串号
    #[serde(default)]
    pub last_serial: Option<String>,
    /// 上次使用的类名
    #[serde(default)]
    pub last_app: Option<String>,
    /// 上次使用的分辨率
    #[serde(default)]
    pub last_resolution: Option<String>,
    /// 上次使用的显示名
    #[serde(default)]
    pub last_app_label: Option<String>,
    /// 屏蔽 IP 格式设备（同一手机只显示 mDNS 串号；没有对应串号的纯 IP 设备折叠显示）
    #[serde(default = "default_true")]
    pub hide_ip_devices: bool,
    /// 分身投屏模式：false=虚拟显示器（手机屏幕不被占用，但系统手势不可用，
    /// 需用 scrcpy 快捷键代替：右键=返回、Alt/Super+H=桌面、Alt/Super+S=最近任务）；
    /// true=直接镜像（分身应用在手机前台启动并镜像到 scrcpy，系统手势可用，
    /// 但手机屏幕会被应用占用）。与 escrcpy 的"直接镜像/新显示器"两种模式对应。
    #[serde(default)]
    pub clone_direct_mirror: bool,
    /// 设备串号 -> 该设备的全部 profile（多对一）
    #[serde(default)]
    pub profiles: HashMap<String, Vec<Profile>>,
    /// 当前激活的 (设备串号, profile 名)
    #[serde(default)]
    pub active_profile: Option<(String, String)>,
}

/// 从设备串号提取物理串号（用于重新配对后 mDNS 串号变化的 profile 迁移匹配）。
/// `adb-AN6B024B01029411-WN3TyX._adb-tls-connect._tcp` -> `AN6B024B01029411`；
/// `adb-AN6B024B01029411-xxxx._adb-tls-connect._tcp:39123`（带端口） -> `AN6B024B01029411`；
/// `AN6B024B01029411`（USB/物理串号）原样返回。
pub fn physical_serial(serial: &str) -> String {
    let s = serial.split(':').next().unwrap_or(serial);
    if let Some(rest) = s.strip_prefix("adb-") {
        let inner = rest.split("._adb-tls-").next().unwrap_or(rest);
        inner
            .rsplit_once('-')
            .map(|(p, _)| p.to_string())
            .unwrap_or_else(|| inner.to_string())
    } else {
        s.to_string()
    }
}

fn default_true() -> bool {
    true
}

fn default_window_width() -> u32 {
    1080
}
fn default_window_height() -> u32 {
    1920
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_aliases: HashMap::new(),
            app_history: Vec::new(),
            app_labels: HashMap::new(),
            resolutions: Vec::new(),
            window_width: default_window_width(),
            window_height: default_window_height(),
            scrcpy_dir: None,
            last_serial: None,
            last_app: None,
            last_resolution: None,
            last_app_label: None,
            hide_ip_devices: true,
            clone_direct_mirror: false,
            profiles: HashMap::new(),
            active_profile: None,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gscrcpy")
            .join("config.json")
    }

    pub fn load() -> Self {
        let p = Self::path();
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(c) = serde_json::from_str(&s) {
                return c;
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }

    /// 旧版默认窗口参数迁移：旧默认 1080x1920（竖屏）与 scrcpy 默认 1920x1080（横屏）
    /// 比例相反，直接启动会出现画面只占一部分。检测到旧默认值则迁移为 1920x1080。
    /// 返回是否发生了迁移（调用方负责保存）。
    pub fn migrate_bad_defaults(&mut self) -> bool {
        if self.window_width == 1080 && self.window_height == 1920 {
            self.window_width = 1920;
            self.window_height = 1080;
            return true;
        }
        false
    }

    /// 设备别名（忽略空字符串）
    pub fn alias(&self, serial: &str) -> Option<&str> {
        self.device_aliases
            .get(serial)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    /// 设备显示名：别名 > 型号 > 串号
    pub fn display_name(&self, serial: &str, model: Option<&str>) -> String {
        self.alias(serial)
            .map(str::to_string)
            .unwrap_or_else(|| model.unwrap_or(serial).to_string())
    }

    /// 向列表头部插入去重项，保留最多 cap 个
    pub fn push_unique(list: &mut Vec<String>, item: String, cap: usize) {
        if let Some(i) = list.iter().position(|x| *x == item) {
            list.remove(i);
        }
        list.insert(0, item);
        list.truncate(cap);
    }

    /// 某设备的全部 profile（不存在时返回空列表）
    pub fn profiles_for(&self, serial: &str) -> &[Profile] {
        self.profiles
            .get(serial)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 某设备 profile 的可变引用；不存在时返回 None（调用方决定是否创建默认）
    pub fn profiles_for_mut(&mut self, serial: &str) -> Option<&mut Vec<Profile>> {
        self.profiles.get_mut(serial)
    }

    /// 取某设备激活的 profile；未激活或缺失时回退到第一个
    pub fn active_profile_for(&self, serial: &str) -> Option<&Profile> {
        let ps = self.profiles_for(serial);
        if ps.is_empty() {
            return None;
        }
        if let Some((s, name)) = &self.active_profile {
            if s == serial {
                if let Some(p) = ps.iter().find(|p| &p.name == name) {
                    return Some(p);
                }
            }
        }
        ps.first()
    }

    /// 把某设备的 profiles 复制到另一串号（重新配对后 mDNS 串号后缀会变，
    /// 按物理串号匹配复制，避免设备重连后"显示新的空白 profile"）。
    /// 旧串号条目保留（USB/旧连接仍可用）。active_profile 同步指向新串号。
    /// 仅当目标串号尚不存在 profiles 时复制（幂等）。返回是否发生复制（调用方负责保存）。
    pub fn copy_profiles(&mut self, from: &str, to: &str) -> bool {
        if from == to {
            return false;
        }
        if self.profiles.contains_key(to) {
            return false;
        }
        let Some(list) = self.profiles.get(from).cloned() else {
            return false;
        };
        self.profiles.insert(to.to_string(), list);
        if let Some((s, name)) = &self.active_profile {
            if s == from {
                self.active_profile = Some((to.to_string(), name.clone()));
            }
        }
        true
    }

    /// 把某设备的 profiles 复制到另一串号（重新配对后 mDNS 串号后缀会变，
    /// 按物理串号匹配复制，避免设备重连后"显示新的空白 profile"）。
    /// 旧串号条目保留（USB/旧连接仍可用）。active_profile 同步指向新串号。
    /// 仅当目标串号尚不存在 profiles 时复制（幂等）。返回是否发生复制（调用方负责保存）。

    /// 把旧版"上次启动参数"迁移进该设备的默认 profile（仅当设备还没有 profile 时）。
    /// 返回是否迁移（调用方负责保存）。
    pub fn migrate_legacy_into_default_profile(&mut self, serial: &str) -> bool {
        if self.profiles.contains_key(serial) {
            return false;
        }
        let mut p = Profile::default();
        p.app = self.last_app.clone().unwrap_or_default();
        p.app_label = self.last_app_label.clone().unwrap_or_default();
        p.resolution = self.last_resolution.clone().unwrap_or_default();
        // 旧版没有分身信息记录，默认按机主处理
        p.clone_user = None;
        self.profiles.insert(serial.to_string(), vec![p]);
        self.active_profile = Some((serial.to_string(), "默认".to_string()));
        true
    }

    /// 把全部 profile 的手势热区模式统一改为 Auto（用户要求"每次自动执行，不要询问"）。
    /// 非荣耀设备执行时会被跳过，无副作用。返回是否发生改动（调用方负责保存）。
    pub fn migrate_all_gesture_auto(&mut self) -> bool {
        let mut changed = false;
        for list in self.profiles.values_mut() {
            for p in list.iter_mut() {
                if p.gesture_fix != GestureFixMode::Auto {
                    p.gesture_fix = GestureFixMode::Auto;
                    changed = true;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut c = Config::default();
        c.device_aliases.insert("abc".into(), "我的手机".into());
        c.app_history.push("com.gof.china".into());
        // profile 持久化
        let mut p = Profile::default();
        p.name = "无尽冬日".into();
        p.app = "com.gof.china".into();
        p.clone_user = Some(128);
        p.resolution = "1920x1080".into();
        p.app_label = "无尽冬日".into();
        p.gesture_fix = GestureFixMode::Auto;
        c.profiles.insert("abc".into(), vec![p.clone()]);
        c.active_profile = Some(("abc".into(), "无尽冬日".into()));
        let s = serde_json::to_string(&c).unwrap();
        let c2: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(c2.alias("abc"), Some("我的手机"));
        assert_eq!(c2.window_width, 1080);
        assert!(c2.hide_ip_devices);
        assert!(!c2.clone_direct_mirror);
        assert_eq!(c2.profiles_for("abc").len(), 1);
        assert_eq!(c2.profiles_for("abc")[0], p);
        assert_eq!(
            c2.active_profile_for("abc").unwrap().name,
            "无尽冬日"
        );
        assert!(c2.profiles_for("missing").is_empty());
        assert!(c2.active_profile_for("missing").is_none());
    }

    #[test]
    fn push_unique_works() {
        let mut l = vec!["a".into(), "b".into()];
        Config::push_unique(&mut l, "c".into(), 2);
        assert_eq!(l, vec!["c", "a"]);
        Config::push_unique(&mut l, "b".into(), 2);
        assert_eq!(l, vec!["b", "c"]);
    }

    #[test]
    fn migrate_bad_defaults_swaps() {
        let mut c = Config::default();
        assert_eq!((c.window_width, c.window_height), (1080, 1920));
        assert!(c.migrate_bad_defaults());
        assert_eq!((c.window_width, c.window_height), (1920, 1080));
        // 幂等：再迁移一次无变化
        assert!(!c.migrate_bad_defaults());
        // 非旧默认值不动
        let mut c2 = Config::default();
        c2.window_width = 1260;
        c2.window_height = 2800;
        assert!(!c2.migrate_bad_defaults());
    }

    #[test]
    fn active_profile_fallback() {
        let mut c = Config::default();
        // 无 profile
        assert!(c.active_profile_for("s").is_none());
        // 有 profile 但未激活 -> 回退第一个
        let mut p1 = Profile::default();
        p1.name = "A".into();
        let mut p2 = Profile::default();
        p2.name = "B".into();
        c.profiles.insert("s".into(), vec![p1, p2]);
        assert_eq!(c.active_profile_for("s").unwrap().name, "A");
        // 激活 B
        c.active_profile = Some(("s".into(), "B".into()));
        assert_eq!(c.active_profile_for("s").unwrap().name, "B");
        // 激活名不存在 -> 回退第一个
        c.active_profile = Some(("s".into(), "X".into()));
        assert_eq!(c.active_profile_for("s").unwrap().name, "A");
    }

    #[test]
    fn physical_serial_parses() {
        // 重新配对后 mDNS 串号后缀变化，物理串号不变
        assert_eq!(
            physical_serial("adb-AN6B024B01029411-WN3TyX._adb-tls-connect._tcp"),
            "AN6B024B01029411"
        );
        assert_eq!(
            physical_serial("adb-AN6B024B01029411-xxxx._adb-tls-connect._tcp:39123"),
            "AN6B024B01029411"
        );
        assert_eq!(physical_serial("AN6B024B01029411"), "AN6B024B01029411");
    }

    #[test]
    fn copy_profiles_works() {
        let mut c = Config::default();
        let old = "adb-ABC123-aaaa._adb-tls-connect._tcp";
        let new = "adb-ABC123-bbbb._adb-tls-connect._tcp";
        c.profiles.insert(
            old.into(),
            vec![Profile {
                name: "无尽冬日".into(),
                ..Default::default()
            }],
        );
        c.active_profile = Some((old.into(), "无尽冬日".into()));
        assert!(c.copy_profiles(old, new));
        assert_eq!(c.profiles_for(new).len(), 1);
        assert_eq!(c.profiles_for(new)[0].name, "无尽冬日");
        assert_eq!(c.active_profile, Some((new.into(), "无尽冬日".into())));
        // 幂等：目标已存在不再复制
        assert!(!c.copy_profiles(old, new));
        // 旧串号保留（USB/旧连接仍可用）
        assert_eq!(c.profiles_for(old).len(), 1);
    }



    #[test]
    fn legacy_migrate_into_profile() {
        let mut c = Config::default();
        c.last_app = Some("com.gof.china".into());
        c.last_resolution = Some("1920x1080".into());
        c.last_app_label = Some("无尽冬日".into());
        assert!(c.migrate_legacy_into_default_profile("s1"));
        let p = c.active_profile_for("s1").unwrap();
        assert_eq!(p.app, "com.gof.china");
        assert_eq!(p.resolution, "1920x1080");
        assert_eq!(p.app_label, "无尽冬日");
        assert_eq!(p.clone_user, None);
        // 已有 profile 不再迁移
        assert!(!c.migrate_legacy_into_default_profile("s1"));
    }

    #[test]
    fn migrate_all_gesture_auto() {
        let mut c = Config::default();
        let mut p1 = Profile::default();
        p1.name = "A".into();
        p1.gesture_fix = GestureFixMode::Off;
        let mut p2 = Profile::default();
        p2.name = "B".into();
        p2.gesture_fix = GestureFixMode::Manual;
        c.profiles.insert("s".into(), vec![p1, p2]);
        assert!(c.migrate_all_gesture_auto());
        assert!(c.profiles_for("s").iter().all(|p| p.gesture_fix == GestureFixMode::Auto));
        // 幂等：已全部 Auto 时不再改动
        assert!(!c.migrate_all_gesture_auto());
    }
}
