use std::io::BufRead;
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

    /// 按用户参数模板拼装启动参数（机主应用 / 直接镜像场景）。
    ///
    /// 模板：scrcpy -s "<serial>" [--new-display=<WxH>] [--start-app=<pkg>]
    ///        --window-width=<w> --window-height=<h> --window-title="<title>"
    ///
    /// - `resolution` 为空 = 直接镜像物理屏幕（不加 --new-display），
    ///   画面比例自动匹配，不会出现 app 只占一部分。
    /// - `start_app`：分身应用场景传 None（scrcpy 4.x 不支持 --user，
    ///   分身由上层 `am start --user <id>` 启动，scrcpy 只负责投屏）。
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
        ];
        if !resolution.is_empty() {
            args.push(format!("--new-display={resolution}"));
        }
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

    /// 分身虚拟显示器投屏参数：总是创建虚拟显示器 `--new-display=<resolution>`，
    /// 不传 `--start-app`（分身应用由上层解析出组件后
    /// `am start-activity --user N --display <id> -n <component>` 启动到该显示器）。
    pub fn build_clone_args(
        &self,
        serial: &str,
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

    /// 异步启动 scrcpy：无控制台窗口，日志追加到配置目录 scrcpy.log。
    /// 返回子进程句柄，调用方可等待其退出（用于投屏结束后自动手势修复）。
    pub fn launch(&self, args: &[String]) -> Result<std::process::Child, String> {
        let mut cmd = Command::new(self.exe());
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
            .map_err(|e| format!("启动 scrcpy 失败: {e}"))
    }

    /// 创建虚拟显示器并投屏（分身场景）。
    ///
    /// 等待 scrcpy 在 stdout 输出 `displayId: N`（虚拟显示器创建成功的唯一标志），
    /// 解析返回显示器 ID，供 `am start-activity --display <id>` 使用。
    /// displayId 每次启动都动态变化，必须实时解析，不能硬编码。
    /// 创建虚拟显示器并投屏（分身场景），返回显示器 ID 与子进程句柄：
    /// ID 供 `am start-activity --display <id>` 使用，句柄供等待退出
    /// （投屏结束后自动手势修复）。displayId 每次动态分配，必须实时解析。
    pub fn launch_with_new_display_child(
        &self,
        args: &[String],
    ) -> Result<(i32, std::process::Child), String> {
        self.spawn_with_display(args)
    }

    /// 内部实现：spawn scrcpy 并解析虚拟显示器 ID，成功返回 `(display_id, child)`。
    /// child 由调用方决定继续持有（正常投屏，drop 后进程保持运行）或立即结束
    /// （手势热区"物理化"修复）。
    fn spawn_with_display(&self, args: &[String]) -> Result<(i32, std::process::Child), String> {
        let mut cmd = Command::new(self.exe());
        cmd.args(args);
        cmd.stdin(Stdio::null());
        // 必须用管道接管 stdout，否则 child.stdout 为 None（继承），
        // 无法实时解析虚拟显示器 ID（displayId）
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.env("ADB", self.adb());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 scrcpy 失败: {e}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "无法读取 scrcpy 输出".to_string())?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            let mut recent: Vec<String> = Vec::new();
            for line in reader.lines() {
                let line = line.unwrap_or_default();
                if let Some(id) = parse_display_id(&line) {
                    let _ = tx.send(Ok(id));
                    return;
                }
                recent.push(line);
                if recent.len() > 20 {
                    recent.remove(0);
                }
            }
            let tail = recent.last().cloned().unwrap_or_else(|| "(无输出)".to_string());
            let _ = tx.send(Err(format!(
                "scrcpy 未输出 displayId，退出前最后输出: {tail}"
            )));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(25)) {
            Ok(Ok(id)) => Ok((id, child)),
            Ok(Err(e)) => {
                let _ = child.kill();
                Err(e)
            }
            Err(_) => {
                let _ = child.kill();
                Err(
                    "等待虚拟显示器创建超时（25 秒）：scrcpy 未能创建显示器。可能原因：adb 连接已断开、设备未解锁、或分辨率参数不被支持。建议手动运行同一条 scrcpy 命令查看输出。"
                        .to_string(),
                )
            }
        }
    }

    /// 荣耀/华为设备专用：把 SystemUI 手势热区"物理化"（手动触发）。
    ///
    /// MagicOS 实测：虚拟显示器按非物理分辨率（如 1920x1280）创建后，系统会把主屏
    /// 手势热区（GestureNav）注册成虚拟显示器分辨率，导致手机本机部分区域手势失效
    /// （如底部上滑/右侧滑动无效）。创建一次物理分辨率虚拟显示器再立即结束，手势
    /// 热区即重新注册为物理屏幕尺寸并固化——即使其他虚拟显示器继续投屏、或之后
    /// 全部移除，热区也保持物理尺寸，手机手势恢复正常，无需重启手机。
    ///
    /// 由用户在"手机触控/手势不正常"时手动调用。
    pub fn repair_gesture_hotzone(&self, serial: &str, w: u32, h: u32) -> Result<(), String> {
        let tmp_rec = std::env::temp_dir().join(format!(
            "gscrcpy_gesture_repair_{}_{}x{}.mp4",
            std::process::id(),
            w,
            h
        ));
        let _ = std::fs::remove_file(&tmp_rec);
        let args = vec![
            "-s".to_string(),
            serial.to_string(),
            format!("--new-display={w}x{h}"),
            "--no-window".to_string(),
            "--no-audio".to_string(),
            format!("--record={}", tmp_rec.display()),
        ];
        // 物理尺寸虚拟显示器创建成功即完成热区注册，立即结束 scrcpy 移除显示器
        let (_, mut child) = self.spawn_with_display(&args)?;
        let _ = child.kill();
        let _ = std::fs::remove_file(&tmp_rec);
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

/// 从 scrcpy stdout 行解析虚拟显示器 ID。
/// scrcpy 4.x 创建虚拟显示器成功后输出形如 `[server] INFO: New display: 1920x1080 (id=2)`
/// （escrcpy 用正则 `/New display:.+?\(id=(\d+)\)/i` 匹配）；
/// 旧版（3.x）输出形如 `[server] INFO: displayId: 2`，两种格式都兼容。
fn parse_display_id(line: &str) -> Option<i32> {
    let line = line.trim();
    // scrcpy 4.x 格式：New display: <w>x<h> (id=<N>)
    if line.to_ascii_lowercase().contains("new display") {
        if let Some(idx) = line.rfind("(id=") {
            let rest = &line[idx + "(id=".len()..];
            return rest.trim_end_matches(')').trim().parse::<i32>().ok();
        }
    }
    // 旧版格式：[server] INFO: displayId: 2
    if let Some(idx) = line.rfind("displayId:") {
        let rest = &line[idx + "displayId:".len()..];
        return rest.trim().parse::<i32>().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn s() -> Scrcpy {
        Scrcpy {
            dir: PathBuf::from("D:/x"),
        }
    }

    #[test]
    fn args_template() {
        let args = s().build_args(
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
        let args = s().build_args("s", Some("p"), "1280x720", 0, 0, "");
        assert!(!args.iter().any(|a| a.contains("window-width")));
        assert!(!args.iter().any(|a| a.contains("window-title")));
    }

    #[test]
    fn args_no_start_app_for_clone_user() {
        // 分身场景：不传 --start-app（由 am start --user 提前启动）
        let args = s().build_args("s", None, "1280x720", 100, 200, "分身 - 华为");
        assert!(!args.iter().any(|a| a.contains("--start-app")));
        assert!(args.contains(&"--new-display=1280x720".to_string()));
    }

    #[test]
    fn args_empty_resolution_direct_mirror() {
        // 空分辨率 = 直接镜像物理屏幕：不加 --new-display
        let args = s().build_args("s", Some("p"), "", 0, 0, "t");
        assert!(!args.iter().any(|a| a.contains("--new-display")));
        assert!(args.contains(&"-s".to_string()));
    }

    #[test]
    fn clone_args_always_new_display() {
        // 分身虚拟显示器：总是 --new-display，且不传 --start-app
        let args = s().build_clone_args("s", "1920x1080", 1920, 1080, "无尽冬日 - Magic");
        assert!(args.contains(&"--new-display=1920x1080".to_string()));
        assert!(!args.iter().any(|a| a.contains("--start-app")));
        assert!(args.contains(&"--window-width=1920".to_string()));
        assert!(args.contains(&"--window-title=无尽冬日 - Magic".to_string()));
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

    #[test]
    fn parse_display_id_from_line() {
        // scrcpy 4.x 格式：New display: <w>x<h> (id=<N>)
        assert_eq!(
            parse_display_id("[server] INFO: New display: 1920x1080 (id=2)"),
            Some(2)
        );
        assert_eq!(
            parse_display_id("[server] INFO: New display: 1280x2800 (id=5)"),
            Some(5)
        );
        // 旧版格式：[server] INFO: displayId: 2
        assert_eq!(parse_display_id("[server] INFO: displayId: 2"), Some(2));
        assert_eq!(parse_display_id("INFO: displayId: 5"), Some(5));
        // 噪声行
        assert_eq!(parse_display_id("scrcpy 4.1 <https://github.com/Genymobile/scrcpy>"), None);
        assert_eq!(parse_display_id(""), None);
    }
}
