use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::adb::CREATE_NO_WINDOW;

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
    /// 模板：scrcpy -s "<serial>" --new-display=<WxH> --start-app=<pkg>
    ///        --window-width=<w> --window-height=<h> --window-title="<title>"
    ///
    /// 注意：scrcpy 4.x 的长选项一律要求 `--opt=value` 形式（等号），
    /// 不能拆成两个参数；短选项 `-s` 用空格分隔。
    pub fn build_args(
        &self,
        serial: &str,
        pkg: &str,
        resolution: &str,
        win_w: u32,
        win_h: u32,
        title: &str,
    ) -> Vec<String> {
        let mut args = vec![
            "-s".to_string(),
            serial.to_string(),
            format!("--new-display={resolution}"),
            format!("--start-app={pkg}"),
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

    /// 异步启动 scrcpy：无控制台窗口，日志追加到配置目录 scrcpy.log
    pub fn launch(&self, args: &[String]) -> Result<(), String> {
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
            .map_err(|e| format!("启动 scrcpy 失败: {e}"))?;
        Ok(())
    }
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
            "com.gof.china",
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
        let args = s.build_args("s", "p", "1280x720", 0, 0, "");
        assert!(!args.iter().any(|a| a.contains("window-width")));
        assert!(!args.iter().any(|a| a.contains("window-title")));
    }
}
