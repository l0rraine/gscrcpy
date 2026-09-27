# gscrcpy — scrcpy 启动器

基于 Rust（egui）的 PC 端工具，用于管理 adb 无线设备并一键用 scrcpy 打开指定应用。

## 功能

- **二维码无线配对**：生成 AOSP 格式配对二维码，手机「无线调试 → 使用二维码配对设备」扫码即可自动配对并连接
- **设备管理**：显示 `adb devices`，可连接/断开选中设备、复制串号、重命名（别名持久化）
- **应用列表**：列出设备全部应用，点击即复制包名并填入启动框
- **一键启动 scrcpy**：按模板拼装参数，支持虚拟分辨率、窗口尺寸、自定义窗口标题（`应用名 - 设备名`）
- **配置持久化**：类名、显示名、分辨率、窗口尺寸、设备别名自动保存（`%APPDATA%/gscrcpy/config.json`）
- **单独更新 scrcpy**：从 GitHub Releases 检查/下载/安装最新版（scrcpy 压缩包自带 adb，一并管理）

## 启动参数模板

```
scrcpy -s "<serial>" --new-display=<WxH> --start-app=<pkg> \
       --window-width=<w> --window-height=<h> --window-title="<显示名> - <设备名>"
```

## 构建

需要 Rust 工具链（1.95+）。

```bash
cargo build --release
# 产物：target/release/gscrcpy.exe
```

## 使用

1. 首次启动若未发现 adb/scrcpy，点击「下载并安装 scrcpy（含 adb）」，工具会安装到 exe 旁的 `tools/` 目录
2. 手机开启「开发者选项 → 无线调试」，点击「生成配对二维码」，用手机扫码完成配对
3. 左侧选择设备，中间填类名/分辨率，点「打开 scrcpy」

## 说明

- `--new-display`（虚拟显示器）需要 Android 12+
- 无线配对要求手机与电脑在同一局域网，且 mDNS（UDP 5353）未被防火墙拦截
- scrcpy 运行日志写入 `%APPDATA%/gscrcpy/scrcpy.log`
- 更新 scrcpy 走 GitHub Releases；国内网络若下载慢，可手动下载 zip 后用「选择 scrcpy 目录…」指定
