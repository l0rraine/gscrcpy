use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::adb::CREATE_NO_WINDOW;

/// 一个应用（来自 `scrcpy --list-apps`，名字是手机端显示名）
#[derive(Debug, Clone)]
pub struct AppInfo {
    pub name: String,
    pub package: String,
    #[allow(dead_code)] // 系统/第三方标记，暂未用于 UI
    pub is_system: bool,
}

pub struct Scrcpy {
    /// 包含 scrcpy.exe（及同目录 adb.exe）的文件夹
    pub dir: PathBuf,
}

impl Scrcpy {
    pub fn exe(&self) -> PathBuf {
        self.dir.join("scrcpy.exe")
    }

    pub fn adb(&self) -> PathBuf {
        self.dir.join("adb.exe")
    }

    pub fn version(&self) -> Option<String> {
        let mut cmd = Command::new(self.exe());
        cmd.arg("--version");
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = cmd.output().ok()?;
        let t = String::from_utf8_lossy(&out.stdout);
        t.lines().next().map(|l| l.trim().to_string())
    }

    /// 按用户参数模板拼装启动参数
    ///
    /// 模板：scrcpy -s "<serial>" --new-display=<WxH> [--start-app=<pkg>]
    ///        --window-width=<w> --window-height=<h> --window-title="<title>"
    ///
    /// `start_app`：分身应用场景传 None（scrcpy 4.x 不支持 --user，
    /// 分身由上层先 `am start --user <id>` 启动，scrcpy 只负责投屏）。
    ///
    /// 注意：scrcpy 4.x 的长选项一律要求 `--opt=value` 形式（等号），
    /// 不能拆成两个参数；短选项 `-s` 用空格分隔。
    pub fn build_args(
        &self,
        serial: &str,
        start_app: Option<&str>,
        resolution: &str,
        win_w: u32,
        win_h: u32,
        title: &str,
    ) -> Vec<String> {
        let mut args = vec![
            "-s".to_string(),
            serial.to_string(),
            format!("--new-display={resolution}"),
        ];
        if let Some(pkg) = start_app {
            args.push(format!("--start-app={pkg}"));
        }
        if win_w > 0 {
            args.push(format!("--window-width={win_w}"));
        }
        if win_h > 0 {
            args.push(format!("--window-height={win_h}"));
        }
        if !title.is_empty() {
            args.push(format!("--window-title={title}"));
        }
        args
    }

    /// 列出设备全部应用（含手机端显示名）。
    /// 复用 scrcpy 内置的 `--list-apps`：一次调用即返回全部包名 + 应用名，
    /// 无需逐包解析 APK；输出形如：
    /// ```text
    ///  * 设置                          com.android.settings
    ///  - 无尽冬日                        com.gof.china
    /// ```
    /// ` * ` 前缀为系统应用，` - ` 为第三方应用，应用名与包名之间用最后一个空格分隔。
    pub fn list_apps(&self, serial: &str) -> Vec<AppInfo> {
        let mut cmd = Command::new(self.exe());
        cmd.arg(format!("--serial={serial}"));
        cmd.arg("--list-apps");
        cmd.stdin(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = match cmd.output() {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        if !out.status.success() {
            return vec![];
        }
        let text = String::from_utf8_lossy(&out.stdout);
        parse_app_list_output(&text)
    }

    /// 异步启动 scrcpy：无控制台窗口，日志追加到配置目录 scrcpy.log
    pub fn launch(&self, args: &[String]) -> Result<(), String> {        let mut cmd = Command::new(self.exe());
        cmd.args(args);
        cmd.stdin(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        // 日志文件
        let log_dir = crate::config::Config::path()
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let _ = std::fs::create_dir_all(&log_dir);
        let log = log_dir.join("scrcpy.log");
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            if let Ok(f1) = f.try_clone() {
                cmd.stdout(Stdio::from(f1));
            }
            if let Ok(f2) = f.try_clone() {
                cmd.stderr(Stdio::from(f2));
            }
        }

        // 指定使用与 scrcpy 同目录的 adb
        cmd.env("ADB", self.adb());
        cmd.spawn()
            .map_err(|e| format!("启动 scrcpy 失败: {e}"))?;
        Ok(())
    }
}

// ---------- 纯解析函数 ----------

/// 解析 `scrcpy --list-apps` 输出。
/// ` * ` 开头为系统应用，` - ` 开头为第三方应用；
/// 应用名与包名间以（对齐用的）多个空格分隔，取最后一个空格切分。
fn parse_app_list_output(out: &str) -> Vec<AppInfo> {
    let mut list = Vec::new();
    for line in out.lines() {
        let (is_system, rest) = if let Some(r) = line.strip_prefix(" * ") {
            (true, r)
        } else if let Some(r) = line.strip_prefix(" - ") {
            (false, r)
        } else {
            continue;
        };
        let rest = rest.trim();
        // 取最后一个空格分割：左边是应用名，右边是包名
        let Some((name, package)) = rest.rsplit_once(' ') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || package.is_empty() {
            continue;
        }
        list.push(AppInfo {
            name: name.to_string(),
            package: package.to_string(),
            is_system,
        });
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn args_template() {
        let s = Scrcpy {
            dir: PathBuf::from("D:/x"),
        };
        let args = s.build_args(
            "adb-D1222091020A-aWsoaY._adb-tls-connect._tcp",
            Some("com.gof.china"),
            "1920x1080",
            1080,
            1920,
            "com.gof.china - 小米14",
        );
        let joined = args.join(" ");
        assert!(joined.contains(r#"-s adb-D1222091020A-aWsoaY._adb-tls-connect._tcp"#));
        assert!(joined.contains("--new-display=1920x1080"));
        assert!(joined.contains("--start-app=com.gof.china"));
        assert!(joined.contains("--window-width=1080"));
        assert!(joined.contains("--window-height=1920"));
        assert!(joined.contains("--window-title=com.gof.china - 小米14"));
    }

    #[test]
    fn args_omit_zero_size() {
        let s = Scrcpy {
            dir: PathBuf::from("D:/x"),
        };
        let args = s.build_args("s", Some("p"), "1280x720", 0, 0, "");
        assert!(!args.iter().any(|a| a.contains("window-width")));
        assert!(!args.iter().any(|a| a.contains("window-title")));
    }

    #[test]
    fn args_no_start_app_for_clone_user() {
        let s = Scrcpy {
            dir: PathBuf::from("D:/x"),
        };
        // 分身场景：不传 --start-app（由 am start --user 提前启动）
        let args = s.build_args("s", None, "1280x720", 100, 200, "分身 - 华为");
        assert!(!args.iter().any(|a| a.contains("--start-app")));
        assert!(args.contains(&"--new-display=1280x720".to_string()));
    }

    #[test]
    fn parse_app_list() {
        let sample = "\
[server] INFO: List of apps:
 * 设置                          com.android.settings
 * 图库                           com.hihonor.photos
 - 无尽冬日                         com.gof.china
 - 微信                            com.tencent.mm
 - 重返未来：1999                   com.shenlan.m.reverse1999
";
        let apps = parse_app_list_output(sample);
        assert_eq!(apps.len(), 5);
        assert_eq!(apps[0].name, "设置");
        assert_eq!(apps[0].package, "com.android.settings");
        assert!(apps[0].is_system);
        assert_eq!(apps[2].name, "无尽冬日");
        assert_eq!(apps[2].package, "com.gof.china");
        assert!(!apps[2].is_system);
        // INFO 行被跳过
        assert!(apps.iter().all(|a| a.name != "[server] INFO: List of apps:"));
    }
}
