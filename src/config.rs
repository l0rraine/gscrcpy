use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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
    /// 设备连接上之后自动重置手势导航（修复部分机型无线调试后侧滑/底部滑动失效）
    #[serde(default = "default_true")]
    pub restore_gesture: bool,
    /// 屏蔽 IP 格式设备（同一手机只显示 mDNS 串号；没有对应串号的纯 IP 设备折叠显示）
    #[serde(default = "default_true")]
    pub hide_ip_devices: bool,
    /// 分身投屏模式：false=虚拟显示器（手机屏幕不被占用，但系统手势不可用，
    /// 需用 scrcpy 快捷键代替：右键=返回、Alt/Super+H=桌面、Alt/Super+S=最近任务）；
    /// true=直接镜像（分身应用在手机前台启动并镜像到 scrcpy，系统手势可用，
    /// 但手机屏幕会被应用占用）。与 escrcpy 的"直接镜像/新显示器"两种模式对应。
    #[serde(default)]
    pub clone_direct_mirror: bool,
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
            restore_gesture: true,
            hide_ip_devices: true,
            clone_direct_mirror: false,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut c = Config::default();
        c.device_aliases.insert("abc".into(), "我的手机".into());
        c.app_history.push("com.gof.china".into());
        let s = serde_json::to_string(&c).unwrap();
        let c2: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(c2.alias("abc"), Some("我的手机"));
        assert_eq!(c2.window_width, 1080);
        assert!(c2.hide_ip_devices);
        assert!(!c2.clone_direct_mirror);
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
}
