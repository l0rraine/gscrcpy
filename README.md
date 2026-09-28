# gscrcpy — scrcpy 启动器

基于 Rust（egui）的 PC 端工具，用于管理 adb 无线设备并一键用 scrcpy 打开指定应用（支持分身应用投屏到虚拟显示器）。

## 功能

- **二维码无线配对**：生成 AOSP 格式配对二维码，手机「无线调试 → 使用二维码配对设备」扫码即可自动配对并连接（对齐 escrcpy：全新 mDNS 探测、配对失败自动移除、连接超时回退 5555 直连）
- **三栏布局（Profile 体系）**
  - 左栏：设备列表 + 连接/二维码区；设备按串号去重显示，支持「屏蔽 IP 格式设备」（含模拟器，勾选后折叠到「已过滤」区）
  - 中栏：当前设备的多个 Profile 列表，可增加、删除、改名（改名需点按钮进入编辑，避免误操作）
  - 右栏：Profile 编辑 —— 应用选择（下拉显示应用名+包名，分身标注 `[分身]`，可输入过滤）、分辨率、窗口尺寸、窗口标题、启动按钮
- **模拟器自动发现**：每 10 秒探测本机常见模拟器端口（MuMu 7555/16384/16385、通用 5555/5554）并自动连接，打开模拟器后无需手动 `adb connect`
- **Profile 自动迁移**：重新配对后 mDNS 串号后缀变化时，按物理串号自动复制保留 Profile 配置
- **分身应用虚拟显示器投屏**：`--new-display` 创建虚拟显示器并在分身用户（user N）中启动应用，投屏不影响手机主屏
- **荣耀/华为手势热区修复**：虚拟显示器投屏后自动用物理分辨率临时修复主屏手势热区，投屏结束恢复手势
- **配置持久化**：Profile（应用、分辨率、窗口尺寸、标题）、设备别名等自动保存（`%APPDATA%/gscrcpy/config.json`）
- **刷新提速**：adb track-devices 事件驱动（500ms 去抖）+ 10 秒定时兜底
- **单独更新 scrcpy**：从 GitHub Releases 检查/下载/安装最新版（scrcpy 压缩包自带 adb，一并管理）

## 启动参数模板

```
scrcpy -s "<serial>" --new-display=<WxH> --start-app=<pkg> \
       --window-width=<w> --window-height=<h> --window-title="<显示名> - <设备名>"
```

分身应用：虚拟显示器创建后解析 display id，再 `am start-activity --user <N> --display <id> -n <component>` 在分身中启动。

## 构建

需要 Rust 工具链（1.95+）。

```bash
cargo build --release
# 产物：target/release/gscrcpy.exe（含应用图标）
```

## 使用

1. 首次启动若未发现 adb/scrcpy，点击「下载并安装 scrcpy（含 adb）」，工具会安装到 exe 旁的 `tools/` 目录
2. 手机开启「开发者选项 → 无线调试」，点击「生成配对二维码」，用手机扫码完成配对
3. 左栏选择设备 → 中栏新建/选择 Profile → 右栏选应用、设分辨率 → 点「启动 app」或「映射屏幕」

## 说明

- `--new-display`（虚拟显示器）需要 Android 12+
- 无线配对要求手机与电脑在同一局域网，且 mDNS（UDP 5353）未被防火墙拦截
- 虚拟显示器投屏期间系统手势不可用（Android 平台限制）：鼠标右键=返回、Alt/Super+H=桌面、Alt/Super+S=最近任务
- scrcpy 运行日志写入 `%APPDATA%/gscrcpy/scrcpy.log`
- 更新 scrcpy 走 GitHub Releases；国内网络若下载慢，可手动下载 scrcpy-win64 zip，解压到 exe 旁的 `tools/` 目录（程序会自动发现 scrcpy.exe 与 adb.exe）

## 发布新版本

推送 `vx.x.x` 样式的 tag 即触发 GitHub Actions 自动构建并创建 Release（含 exe 与 zip 产物）：

```bash
git tag v1.1.0
git push origin v1.1.0
```

Workflow：`.github/workflows/release.yml`（仅匹配 `v*.*.*` 格式的 tag）。
