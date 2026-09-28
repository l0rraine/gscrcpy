use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Windows: 创建进程时不弹出控制台窗口
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // product 等字段保留备用
pub struct DeviceInfo {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MdnsService {
    /// 实例名，如 "adb-RM3461-xxxx"
    pub instance: String,
    /// 服务类型，如 "_adb-tls-pairing._tcp" / "_adb-tls-connect._tcp"
    pub service: String,
    pub host: String,
    pub port: u16,
}

/// Android 多用户（分身应用等）
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub id: i32,
    pub name: String,
}

pub struct Adb {
    pub path: PathBuf,
}

#[allow(dead_code)] // version/getprop 保留备用
impl Adb {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 运行 adb 命令并返回 stdout（UTF-8，去尾空白）
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut cmd = Command::new(&self.path);
        cmd.args(args);
        cmd.stdin(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = cmd
            .output()
            .map_err(|e| format!("执行 adb 失败: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if err.is_empty() { text } else { err });
        }
        Ok(text)
    }

    pub fn version(&self) -> Result<String, String> {
        self.run(&["version"])
    }

    /// 解析 `adb devices -l` 输出
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let out = match self.run(&["devices", "-l"]) {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        parse_devices_output(&out)
    }

    /// 列出设备上的全部包名（默认 user 0，机主）
    pub fn packages(&self, serial: &str) -> Vec<String> {
        let out = match self.run(&["-s", serial, "shell", "pm", "list", "packages"]) {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        out.lines()
            .filter_map(|l| l.trim().strip_prefix("package:").map(|s| s.to_string()))
            .collect()
    }

    /// 列出设备上的多用户（排除机主 user 0；华为分身=user 128、努比亚=user 999 等）
    pub fn users(&self, serial: &str) -> Vec<UserInfo> {
        let out = match self.run(&["-s", serial, "shell", "pm", "list", "users"]) {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        parse_users_output(&out)
    }

    /// 列出指定用户下的包（分身应用在 user 128/999 等）。
    /// 注意部分设备（如努比亚）不接受 `--user=N` 等号形式，必须用 `--user N` 空格形式。
    pub fn packages_for_user(&self, serial: &str, user: i32) -> Vec<String> {
        let u = user.to_string();
        let out = match self.run(&[
            "-s",
            serial,
            "shell",
            "pm",
            "list",
            "packages",
            "--user",
            &u,
        ]) {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        out.lines()
            .filter_map(|l| l.trim().strip_prefix("package:").map(|s| s.to_string()))
            .collect()
    }

    /// 列出指定用户下的**三方应用**包（`pm list packages -3`，与 escrcpy 一致：
    /// 分身用户里只展示用户自己装的第三方应用，过滤系统噪音）。
    pub fn packages_third_party_for_user(&self, serial: &str, user: i32) -> Vec<String> {
        let u = user.to_string();
        let out = match self.run(&[
            "-s",
            serial,
            "shell",
            "pm",
            "list",
            "packages",
            "-3",
            "--user",
            &u,
        ]) {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        out.lines()
            .filter_map(|l| l.trim().strip_prefix("package:").map(|s| s.to_string()))
            .collect()
    }

    /// 列出指定用户下**有桌面启动入口**的包（`cmd package query-activities`，
    /// 与 escrcpy 一致：只展示可启动的应用，过滤无 Activity 的服务/后台包）。
    pub fn launchable_packages(&self, serial: &str, user: i32) -> Vec<String> {
        let u = user.to_string();
        let out = match self.run(&[
            "-s",
            serial,
            "shell",
            "cmd",
            "package",
            "query-activities",
            "--brief",
            "--components",
            "--user",
            &u,
            "-a",
            "android.intent.action.MAIN",
            "-c",
            "android.intent.category.LAUNCHER",
        ]) {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        // 输出每行形如 "com.tencent.mm/.ui.LauncherUI"（--components 时）
        parse_launchable_output(&out)
    }

    /// 查询设备物理屏幕分辨率（`wm size`），返回 (宽, 高)
    pub fn wm_size(&self, serial: &str) -> Option<(u32, u32)> {
        let out = self.run(&["-s", serial, "shell", "wm", "size"]).ok()?;
        parse_wm_size(&out)
    }

    /// adb mDNS 后端诊断（`adb mdns check`），用于排查无线调试发现问题
    pub fn mdns_check(&self) -> Option<String> {
        self.run(&["mdns", "check"]).ok()
    }

    /// 解析包在指定用户下的启动 Activity（component，如 com.gof.china/.MainActivity）。
    ///
    /// 返回 `Err` 时携带 adb 真实输出，避免误导为"未创建分身"。
    ///
    /// 注意（已在荣耀 Magic user 128 实测）：
    /// - 分身场景必须带 `--brief --components`（与 escrcpy 一致），
    ///   单独 `--brief` 配 `--user` 在 MagicOS 上返回空 → 误报"未找到应用"。
    /// - 部分设备（如努比亚/华为分身场景）不接受 `--user=N` 等号形式，
    ///   必须用 `--user N` 空格形式；这里空格优先，失败时再用等号形式兜底一次。
    pub fn resolve_activity(&self, serial: &str, user: i32, pkg: &str) -> Result<String, String> {
        let u = user.to_string();
        let out = self
            .run(&[
                "-s",
                serial,
                "shell",
                "cmd",
                "package",
                "resolve-activity",
                "--brief",
                "--components",
                "--user",
                &u,
                pkg,
            ])
            .or_else(|_| {
                let eq = format!("--user={user}");
                self.run(&[
                    "-s",
                    serial,
                    "shell",
                    "cmd",
                    "package",
                    "resolve-activity",
                    "--brief",
                    "--components",
                    &eq,
                    pkg,
                ])
            });
        let out = out.map_err(|e| format!("resolve-activity 执行失败: {e}"))?;
        for line in out.lines() {
            let l = line.trim();
            if l.contains('/')
                && !l.starts_with("No activity")
                && !l.starts_with("priority=")
                && !l.starts_with("Warning")
            {
                return Ok(l.to_string());
            }
        }
        Err(format!(
            "在 user {user} 中解析不到 {pkg} 的启动 Activity（adb 输出：{}）",
            out.trim()
        ))
    }

    /// 以指定用户启动应用（分身场景；component 来自 resolve_activity）。
    /// 同样优先空格形式 `--user N`，失败时用等号形式兜底。
    pub fn start_app_for_user(
        &self,
        serial: &str,
        user: i32,
        component: &str,
    ) -> Result<String, String> {
        let u = user.to_string();
        let eq = format!("--user={user}");
        let r = self.run(&["-s", serial, "shell", "am", "start", "--user", &u, "-n", component]);
        match r {
            Ok(o) => {
                if o.contains("Error") || o.contains("Exception") {
                    self.run(&["-s", serial, "shell", "am", "start", &eq, "-n", component])
                } else {
                    Ok(o)
                }
            }
            Err(_) => self.run(&["-s", serial, "shell", "am", "start", &eq, "-n", component]),
        }
    }

    /// 以指定用户把应用启动到**指定虚拟显示器**上（分身投屏，对齐 escrcpy）。
    ///
    /// `display_id` 来自 `scrcpy --new-display` 输出中解析出的虚拟显示器 ID；
    /// `am start-activity --user N --display <id> -n <component>` 让应用渲染到
    /// scrcpy 窗口对应的虚拟显示器，手机主屏不受影响。
    /// 优先空格形式 `--user N --display D`，失败时用等号形式兜底。
    pub fn start_app_for_user_on_display(
        &self,
        serial: &str,
        user: i32,
        component: &str,
        display_id: i32,
    ) -> Result<String, String> {
        let u = user.to_string();
        let d = display_id.to_string();
        let args = [
            "-s",
            serial,
            "shell",
            "am",
            "start-activity",
            "--user",
            &u,
            "--display",
            &d,
            "-n",
            component,
        ];
        let r = self.run(&args);
        match r {
            Ok(o) => {
                if o.contains("Error")
                    || o.contains("Exception")
                    || o.contains("Warning: Activity not started")
                {
                    let eq_u = format!("--user={user}");
                    let eq_d = format!("--display={display_id}");
                    self.run(&[
                        "-s",
                        serial,
                        "shell",
                        "am",
                        "start-activity",
                        &eq_u,
                        &eq_d,
                        "-n",
                        component,
                    ])
                } else {
                    Ok(o)
                }
            }
            Err(_) => {
                let eq_u = format!("--user={user}");
                let eq_d = format!("--display={display_id}");
                self.run(&[
                    "-s",
                    serial,
                    "shell",
                    "am",
                    "start-activity",
                    &eq_u,
                    &eq_d,
                    "-n",
                    component,
                ])
            }
        }
    }

    /// 重置手势导航相关设置（修复部分机型无线调试连接后侧滑/从底部滑动失效）。
    /// 逐条执行并汇总结果；任一命令失败时仍继续执行其余命令。
    pub fn restore_gesture_settings(&self, serial: &str) -> Result<String, String> {
        let cmds: [(&str, &str); 2] = [
            // Android 11+ 手势导航（0=三键，2=手势）
            ("settings put secure navigation_mode 2", "手势导航 navigation_mode=2"),
            // 关闭"显示触摸操作"（escrcpy 等工具可能遗留开启）
            ("settings put system show_touches 0", "关闭显示触摸操作 show_touches=0"),
        ];
        let mut ok = Vec::new();
        let mut failed = Vec::new();
        for (cmd, desc) in cmds {
            match self.run(&["-s", serial, "shell", &cmd]) {
                Ok(_) => ok.push(desc.to_string()),
                Err(e) => failed.push(format!("{desc} 失败: {e}")),
            }
        }
        if ok.is_empty() && !failed.is_empty() {
            return Err(failed.join("；"));
        }
        Ok(format!(
            "已重置: {}。{}",
            ok.join("、"),
            if failed.is_empty() {
                String::new()
            } else {
                failed.join("；")
            }
        ))
    }

    /// 解析 `adb mdns services` 输出
    pub fn mdns_services(&self) -> Vec<MdnsService> {
        let out = match self.run(&["mdns", "services"]) {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        parse_mdns_output(&out)
    }

    /// 无线调试配对
    pub fn pair(&self, host_port: &str, code: &str) -> Result<String, String> {
        self.run(&["pair", host_port, code])
    }

    pub fn connect(&self, host_port: &str) -> Result<String, String> {
        self.run(&["connect", host_port])
    }

    pub fn disconnect(&self, host_port: &str) -> Result<String, String> {
        self.run(&["disconnect", host_port])
    }

    pub fn getprop(&self, serial: &str, prop: &str) -> Option<String> {
        self.run(&["-s", serial, "shell", "getprop", prop])
            .ok()
            .filter(|s| !s.is_empty() && s != "unknown")
    }

    /// 判断是否为荣耀/华为设备（手势热区"物理化"修复只对这类设备必要；
    /// 类原生设备如努比亚不受虚拟显示器分辨率污染影响）
    pub fn is_honor_device(&self, serial: &str) -> bool {
        ["ro.product.brand", "ro.product.manufacturer"]
            .iter()
            .filter_map(|k| self.getprop(serial, k))
            .any(|v| {
                let v = v.to_lowercase();
                v.contains("honor") || v.contains("huawei")
            })
    }
}

// ---------- 纯解析函数 ----------

fn parse_devices_output(out: &str) -> Vec<DeviceInfo> {
    let mut list = Vec::new();
    for line in out.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let serial = it.next().unwrap_or("").to_string();
        let state = it.next().unwrap_or("").to_string();
        let mut model = None;
        let mut product = None;
        for kv in it {
            if let Some((k, v)) = kv.split_once(':') {
                match k {
                    "model" => model = Some(v.to_string()),
                    "product" => product = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        list.push(DeviceInfo {
            serial,
            state,
            model,
            product,
        });
    }
    list
}

/// 解析 `pm list users` 输出，返回除机主(user 0)外的用户。
/// 输出形如：
/// ```
/// Users:
///         UserInfo{0:机主:13} running
///         UserInfo{128:分身应用:4100410} running
/// ```
fn parse_users_output(out: &str) -> Vec<UserInfo> {
    let mut list = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if !line.starts_with("UserInfo{") {
            continue;
        }
        let Some(rest) = line.strip_prefix("UserInfo{") else { continue };
        // rest 形如 "0:机主:13} running"
        let Some(body) = rest.split('}').next() else { continue };
        let mut it = body.splitn(3, ':');
        let Some(id) = it.next().and_then(|s| s.trim().parse::<i32>().ok()) else {
            continue;
        };
        let name = it.next().unwrap_or("").trim().to_string();
        // 跳过机主 user 0
        if id == 0 {
            continue;
        }
        list.push(UserInfo { id, name });
    }
    list
}

fn parse_mdns_output(out: &str) -> Vec<MdnsService> {    let mut list = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let Some(inst) = it.next() else { continue };
        let Some(addr) = it.next() else { continue };
        let (host, port) = match addr.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().unwrap_or(0)),
            None => (addr.to_string(), 0),
        };
        // inst 形如 "adb-xxx._adb-tls-pairing._tcp"
        let (instance, service) = match inst.rsplit_once("._adb-tls") {
            Some((i, rest)) => (i.to_string(), format!("_adb-tls{}", rest)),
            None => (inst.to_string(), String::new()),
        };
        // 跳过表头/无端口等无效行
        if service.is_empty() || port == 0 {
            continue;
        }
        list.push(MdnsService {
            instance,
            service,
            host,
            port,
        });
    }
    list
}

/// 解析 `wm size` 输出，取 Physical size（忽略 Override size）
fn parse_wm_size(out: &str) -> Option<(u32, u32)> {
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Physical size:") {
            let rest = rest.trim();
            if let Some((w, h)) = rest.split_once('x') {
                let w = w.trim().parse::<u32>().ok()?;
                let h = h.trim().parse::<u32>().ok()?;
                if w > 0 && h > 0 {
                    return Some((w, h));
                }
            }
        }
    }
    None
}

/// 解析 `cmd package query-activities --components` 输出。
/// 每行形如 "com.tencent.mm/.ui.LauncherUI"（组件形式），取包名。
fn parse_launchable_output(out: &str) -> Vec<String> {
    let mut list = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 兼容 "package/activity" 与 "package/.Activity" 等形态
        if let Some((p, _)) = line.split_once('/') {
            let p = p.trim().to_string();
            if !p.is_empty() {
                list.push(p);
            }
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_devices() {
        let sample = "\
List of devices attached
adb-D1222091020A-aWsoaY._adb-tls-connect._tcp  device product:chiron model:MI_5 device:chiron transport_id:1
emulator-5554  offline transport_id:2
";
        let devs = parse_devices_output(sample);
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].serial, "adb-D1222091020A-aWsoaY._adb-tls-connect._tcp");
        assert_eq!(devs[0].model.as_deref(), Some("MI_5"));
        assert_eq!(devs[0].product.as_deref(), Some("chiron"));
        assert_eq!(devs[1].state, "offline");
    }

    #[test]
    fn parse_mdns() {
        let sample = "\
List of discovered mdns services:
adb-RM3461-aa11._adb-tls-pairing._tcp  192.168.1.5:37001
adb-RM3461-bb22._adb-tls-connect._tcp  192.168.1.5:37855
";
        let list = parse_mdns_output(sample);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].service, "_adb-tls-pairing._tcp");
        assert_eq!(list[0].instance, "adb-RM3461-aa11");
        assert_eq!(list[0].port, 37001);
        assert_eq!(list[1].service, "_adb-tls-connect._tcp");
        assert_eq!(list[1].instance, "adb-RM3461-bb22");
        assert_eq!(list[1].port, 37855);
    }

    #[test]
    fn parse_users() {
        let sample = "\
Users:
        UserInfo{0:机主:13} running
        UserInfo{128:分身应用:4100410} running
        UserInfo{999:应用分身:4100410} running
";
        let users = parse_users_output(sample);
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].id, 128);
        assert_eq!(users[0].name, "分身应用");
        assert_eq!(users[1].id, 999);
        assert_eq!(users[1].name, "应用分身");
    }

    #[test]
    fn parse_wm_size_physical() {
        let sample = "\
Physical size: 1260x2800
Override size: 1260x2800
";
        assert_eq!(parse_wm_size(sample), Some((1260, 2800)));
        // 只有物理大小
        assert_eq!(parse_wm_size("Physical size: 1080x2400"), Some((1080, 2400)));
        // 无效行
        assert_eq!(parse_wm_size("Physical size: abc"), None);
        assert_eq!(parse_wm_size(""), None);
    }

    #[test]
    fn parse_launchable() {
        let sample = "\
com.tencent.mm/.ui.LauncherUI
com.gof.china/.MainActivity
org.telegram.messenger/.DefaultIcon
";
        let pkgs = parse_launchable_output(sample);
        assert_eq!(pkgs, vec!["com.tencent.mm", "com.gof.china", "org.telegram.messenger"]);
        // 表头/空行被跳过
        assert_eq!(parse_launchable_output("header\n\n"), Vec::<String>::new());
    }
}
