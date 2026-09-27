use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Clone)]
pub struct ReleaseInfo {
    pub tag: String,
    pub win64_zip: String,
}

const LATEST_API: &str = "https://api.github.com/repos/Genymobile/scrcpy/releases/latest";

/// 查询 scrcpy 最新 release
pub fn latest_release() -> Result<ReleaseInfo, String> {
    let mut resp = ureq::get(LATEST_API)
        .header("User-Agent", "gscrcpy")
        .call()
        .map_err(|e| format!("查询 GitHub 最新版本失败: {e}"))?;
    let buf = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("读取响应失败: {e}"))?;
    let v: Value =
        serde_json::from_str(&buf).map_err(|e| format!("解析 GitHub 响应失败: {e}"))?;
    let tag = v["tag_name"].as_str().unwrap_or("unknown").to_string();
    let mut win64_zip = None;
    if let Some(assets) = v["assets"].as_array() {
        for a in assets {
            let name = a["name"].as_str().unwrap_or("");
            if name.contains("win64") && name.ends_with(".zip") {
                win64_zip = a["browser_download_url"].as_str().map(|s| s.to_string());
                break;
            }
        }
    }
    let win64_zip =
        win64_zip.ok_or_else(|| "最新发布中没有找到 win64 zip 资源".to_string())?;
    Ok(ReleaseInfo { tag, win64_zip })
}

/// 比较 "v3.3.4" 形式的版本号
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    fn nums(s: &str) -> Vec<u32> {
        s.trim_start_matches('v')
            .split('.')
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    }
    let (na, nb) = (nums(a), nums(b));
    for i in 0..na.len().max(nb.len()) {
        let x = na.get(i).copied().unwrap_or(0);
        let y = nb.get(i).copied().unwrap_or(0);
        if x != y {
            return x.cmp(&y);
        }
    }
    Ordering::Equal
}

/// 下载文件到 dest
pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    let mut resp = ureq::get(url)
        .header("User-Agent", "gscrcpy")
        .call()
        .map_err(|e| format!("下载失败: {e}"))?;
    let mut reader = resp.body_mut().as_reader();
    let mut out = std::fs::File::create(dest).map_err(|e| format!("创建文件失败: {e}"))?;
    std::io::copy(&mut reader, &mut out).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(())
}

/// 解压 zip 到 target_dir，返回包含 scrcpy.exe 的内层目录
pub fn extract_zip(zip_path: &Path, target_dir: &Path) -> Result<PathBuf, String> {
    let f = std::fs::File::open(zip_path).map_err(|e| format!("打开 zip 失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(f).map_err(|e| format!("解析 zip 失败: {e}"))?;
    let mut inner: Option<PathBuf> = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        // 防路径穿越：只接受普通路径组件
        let rel = Path::new(&name);
        let clean = rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !clean {
            continue;
        }
        let out_path = target_dir.join(rel);
        if entry.is_dir() {
            let _ = std::fs::create_dir_all(&out_path);
            continue;
        }
        if let Some(parent) = out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut fout =
            std::fs::File::create(&out_path).map_err(|e| format!("创建 {} 失败: {e}", name))?;
        std::io::copy(&mut entry, &mut fout).map_err(|e| format!("解压 {} 失败: {e}", name))?;
        if out_path.file_name().map(|n| n == "scrcpy.exe") == Some(true) {
            inner = Some(out_path.parent().unwrap_or(target_dir).to_path_buf());
        }
    }
    inner.ok_or_else(|| "解压后未找到 scrcpy.exe".to_string())
}

/// 下载并安装新版本到 tools 目录，返回最终 scrcpy 目录
pub fn install_new_version(tools_dir: &Path, info: &ReleaseInfo) -> Result<PathBuf, String> {
    let _ = std::fs::create_dir_all(tools_dir);
    let staging = tools_dir.join("staging");
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    let _ = std::fs::create_dir_all(&staging);

    let zip_path = tools_dir.join("scrcpy-download.zip");
    let _ = std::fs::remove_file(&zip_path);
    download(&info.win64_zip, &zip_path)?;

    let inner = extract_zip(&zip_path, &staging)?;

    let final_dir = tools_dir.join(format!(
        "scrcpy-win64-{}",
        info.tag.trim_start_matches('v')
    ));
    if final_dir.exists() {
        let _ = std::fs::remove_dir_all(&final_dir);
    }
    std::fs::rename(&inner, &final_dir).map_err(|e| format!("移动目录失败: {e}"))?;

    let _ = std::fs::remove_file(&zip_path);
    let _ = std::fs::remove_dir_all(&staging);
    Ok(final_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmp_versions() {
        assert_eq!(version_cmp("v3.3.4", "v3.3.4"), Ordering::Equal);
        assert_eq!(version_cmp("v3.3.4", "v3.4.0"), Ordering::Less);
        assert_eq!(version_cmp("v4.1", "v3.9.9"), Ordering::Greater);
        assert_eq!(version_cmp("v2.0", "v1.25"), Ordering::Greater);
    }
}
