# gscrcpy 荣耀 Magic 分身投屏・成功运行经验总结

> 本文档汇总本项目（gscrcpy，Rust + egui 的 scrcpy 图形启动器）在荣耀 Magic 上
> 无线投屏分身应用时，从踩坑到验证有效的全部经验。所有结论均经真机实测确认。



***

## 环境



| 项目   | 值                                                          |
| ---- | ---------------------------------------------------------- |
| 目标手机 | 荣耀 Magic（PTP-AN10 / Android 17 / MagicOS）                  |
| 物理屏幕 | 1280x2800（密度 560）                                          |
| 对比机型 | 努比亚 NX702J（非荣耀，手势热区不受虚拟显示器分辨率污染）                           |
| 分身应用 | 无尽冬日 `com.gof.china`（分身 user 128）                          |
| 分身组件 | `com.gof.china/com.unity3d.player.DDUnityLaunchActivity`   |
| 工具   | scrcpy 4.1 + adb（`target\release\tools\scrcpy-win64-4.1\`） |



***

## 1. 无线配对（二维码 + 配对码）

**现象**：二维码配对失败，旧版日志只有 "已生成配对二维码，请扫码"，没有任何有用信息。

**排查**：对照开源项目 escrcpy 源码（`%TEMP%\escrcpy-src\`，其 `desktop/electron/middleware/scrcpy/index.js` 与 `use-start-app` 钩子）逐步核对配对流程。

**有效解法**（已落地 gscrcpy）：



1. 配对全程记录真实输出 / 错误到 "配对过程日志"，不再只显示笼统提示。

2. 新增**配对码手动配对**入口（二维码失败的可靠兜底）：

* 手机「开发者选项 → 无线调试 → 使用配对码配对设备」页面显示 `ip:port` 与 6 位配对码；

* 命令本质：`adb pair <ip:port> <6位配对码>`；

* 二维码同样 2 分钟内有效，需同一 WiFi。

1. 配对成功后 `adb connect <ip:port>` 建立连接。



***

## 2. 设备列表去重（同一手机只显示串号）

**现象**：同一台手机会同时出现两个条目 —— 一个 `ip:port`（如 `192.168.1.5:37855`）、一个 mDNS 串号（如 `adb-XXXX._adb-tls-connect._tcp`）。

**解法**（已落地）：



* 通过自建 mDNS 缓存把 `ip:port` 反查为 mDNS 串号；若同一手机已存在串号条目，则 ip 条目判为重复并隐藏；

* 默认勾选「屏蔽 IP 格式设备」：没有对应串号的纯 IP 设备也折叠显示；

* 取消勾选后全部显示。



***

## 3. 分身应用显示名字

**现象**：分身应用列表只显示包名，不显示应用名（escrcpy 能显示）。

**解法**（已落地）：



* 优先用 `scrcpy --list-apps` 一次性拿到主用户（user 0）应用的**包名 + 手机端显示名**；

* 分身用户（华为 user 128「分身应用」/ 努比亚 user 999「应用分身」）里**同包名复用主用户的显示名**，与 escrcpy 行为一致。



***

## 4. 无线连接后画面只占一部分

**现象**：scrcpy 直接映射手机屏幕时，能看到 app 内容但只占画面一部分（四周黑边），app 整体又是完整的。

**根因**：旧版默认分辨率参数为 `1920x1080(横) + 1080x1920(竖)`，**宽高写反了**；且请求的虚拟分辨率与设备物理分辨率比例不一致时，app 只渲染在画面的一部分。

**解法**（已落地）：



* 启动时自动迁移旧版反了的默认参数；

* 填虚拟分辨率时，若与设备物理分辨率比例不一致（`rw*ph != rh*pw`），日志明确提示；

* 提供「用设备分辨率 1280x2800」一键按钮；分辨率留空 = 直接镜像物理屏幕，窗口自动匹配比例，永不出现黑边。



***

## 5. 分身启动报 "未找到应用"

**现象**：`am start --user 128 -n <Activity>` 报 "分身 (user 128) 中未找到应用 com.gof.china"。

**根因**：解析 Activity 的命令参数不对。MagicOS 上**单独&#x20;**`--brief`**&#x20;配&#x20;**`--user`**&#x20;会返回空**，导致误判 "未创建分身"。

**有效命令**（已落地，空格形式优先、等号形式兜底）：



```
# 解析分身 Activity（必须带 --brief --components --user N）
adb -s {serial} shell cmd package resolve-activity --brief --components --user 128 com.gof.china

# 启动分身到虚拟显示器（display id 来自 scrcpy --new-display 输出，每次动态变化）
adb -s {serial} shell am start-activity --user 128 --display {id} -n com.gof.china/com.unity3d.player.DDUnityLaunchActivity

# scrcpy 创建虚拟显示器投屏
scrcpy -s {serial} --new-display=1920x1080 --window-width=1920 --window-height=1080 --window-title=无尽冬日 - Magic
```



***

## 6. 核心难点：虚拟显示器投屏导致手机本机手势失效（荣耀 MagicOS 热区污染）

### 6.1 现象与规律



* 用虚拟显示器（非物理分辨率，如 1920x1280）投屏后，**手机本机**部分区域手势失效：底部上滑、右侧滑动无效；

* 不是完全失效：**下半部分失效，屏幕约一半位置仍可左侧滑动**（可触发退出游戏提示）；

* 锁屏解锁、重启 gscrcpy、杀 scrcpy 移除全部虚拟显示器、写回 `navigation_mode`、`force-stop SystemUI` 均**不恢复**，此前只能**重启手机**；

* 努比亚 NX702J 虚拟显示器投屏**不破坏**本机手势 → 问题为荣耀 MagicOS 特有；

* 荣耀 "下午有一阵可以、后来不行"、"添加手势恢复后第一次 / 第二次更改还可以，后来就都不可以" → 是 SystemUI **运行态**被污染，不是设置被改。

### 6.2 根因（dumpsys input 实锤）



* 荣耀 GestureNav 手势热区随 "最近创建 / 活动的虚拟显示器尺寸" 注册；

* 物理屏是 **1280x2800**，虚拟显示器按 **1920x1280** 创建后，热区被注册成虚拟显示器分辨率坐标，与物理屏错位 → 部分区域无手势。

热区基线（重启后健康态）：



```
GestureNavBottom = [0,2660][1280,2800]
GestureNavLeft   = [0,141][53..1280,2800]   # 均为物理尺寸
```

污染态（1920x1280 虚拟显示器投屏后）：



```
GestureNavBottom = [0,1184..1920,1280]      # 缩到虚拟显示器分辨率，物理屏下半部分失效
```

### 6.3 验证无效的手段（避免再走弯路）



| 手段                                                     | 结果           |
| ------------------------------------------------------ | ------------ |
| settings put secure navigation\_mode 0→2 / 强制 0→106 切换 | 无效           |
| user 128 设置 106                                        | 无效           |
| adb kill / pkill SystemUI（Operation not permitted）     | 无效           |
| am force-stop com.android.systemui（进程确实重启）             | 无效           |
| 锁屏解锁                                                   | 无效           |
| 杀全部 scrcpy 移除虚拟显示器                                     | 无效（热区不回退）    |
| wm size 强制变更再 reset / 强制旋转 /nav 模式切换重建监视器              | 无效           |
| **重启手机**                                               | **有效（但代价大）** |

### 6.4 有效解法：物理分辨率虚拟显示器覆盖一次即固化



1. 虚拟显示器投屏（如 1920x1280）造成热区污染；

2. **创建一次物理分辨率（1280x2800）虚拟显示器，再立即结束**；

3. 热区重新注册为物理尺寸并**固化**：之后无论 1920x1280 投屏是否继续、是否再移除全部虚拟显示器，热区保持 1280x2800；

4. 投屏期间与结束后手机手势均正常，**无需重启手机**。

核心命令（手动执行一次）：



```
scrcpy -s {serial} --new-display=1280x2800 --no-window --no-audio --record=%TEMP%\gscrcpy_gesture_repair.mp4
# 等 scrcpy 输出 "New display: ... (id=N)"（4.x 格式；旧版 3.x 为 "displayId: N"）即创建成功，随后关闭 scrcpy 即可
```

### 6.5 落地到 gscrcpy：手动按钮「修复手机手势」



* 用户需求定型为**手动选项**（不自动执行）：手机触控 / 手势不正常时，由用户手动触发；

* 选中设备（在线）→ 连接 / 断开按钮旁点「修复手机手势」：

1. `is_honor_device()`：读 `ro.product.brand` / `ro.product.manufacturer`，仅荣耀 / 华为执行（努比亚等自动跳过）；

2. `wm size` 取物理分辨率；

3. `repair_gesture_hotzone()`：`--new-display=物理尺寸 --no-window --no-audio --record=<临时文件>`，解析到 display id 后立即结束进程并删除临时文件；

* 代码位置：`src/adb.rs`（is\_honor\_device）、`src/scrcpy.rs`（repair\_gesture\_hotzone / spawn\_with\_display）、`src/app.rs`（action\_repair\_gesture + UI 按钮）。



***

## 7. 虚拟显示器 vs 直接镜像（分身投屏两种模式）



| 模式                   | 手机屏幕      | 系统手势                            | 说明                                                   |
| -------------------- | --------- | ------------------------------- | ---------------------------------------------------- |
| 虚拟显示器（默认，对齐 escrcpy） | 不被占用      | **不可用**（平台限制，scrcpy/escrcpy 相同） | 用快捷键代替：鼠标右键 = 返回，Alt/Super+H = 桌面，Alt/Super+S = 最近任务 |
| 直接镜像                 | 被应用占用（前台） | 可用                              | 分身应用在手机前台启动并镜像到 scrcpy                               |



* 虚拟显示器模式下 "app 在后台显示、前台可操作" 是正常表现（nubia 也一样）；

* 分身直接镜像模式：先 `am start --user N -n <component>` 前台启动，再 scrcpy 直接镜像（不建虚拟显示器，分辨率强制留空）。



***

## 8. 分身 App 在虚拟显示器正常显示的完整机制（可复用）

> 这一节是 "分身 App 显示在 scrcpy 虚拟显示器窗口、手机主屏不受影响" 的完整技术原理与
> 可复用实现步骤，任何项目（其他语言的 scrcpy 客户端、脚本、自动化工具）均可照此实现。

### 8.1 概念与原理

Android 支持**多显示器**（DisplayManager 虚拟显示器 virtual display）：应用可以被启动到

指定的 display 上渲染，各显示器内容互相独立。

scrcpy 的显示模式：



| scrcpy 参数           | 含义                                    |
| ------------------- | ------------------------------------- |
| （无）                 | 映射模式：显示设备当前屏幕内容（主屏）                   |
| `--new-display=WxH` | **创建新的虚拟显示器并显示它**，不占用手机主屏             |
| `--display N`       | 显示现有指定 id 的显示器（配合 --new-display 已创建的） |

**核心要点**：`--new-display` 只是 "创建并显示一个空虚拟显示器"；分身 App 必须再用

`am start-activity --display <id>` **显式启动到该虚拟显示器**，否则它会在手机前台

（主屏）启动，scrcpy 窗口显示的将是主屏内容或空白窗口 —— 这就是 " 直接前台启动、

scrcpy 是映射模式、出现空白 scrcpy 窗口 " 的原因。

### 8.2 完整流程（四步，顺序不能颠倒）

**步骤 1：确认分身用户 id**



```
adb -s {serial} shell pm list users
# 输出形如 UserInfo{128:分身应用:4100410} running（华为 user 128「分身应用」/ 努比亚 user 999）
```

**步骤 2：解析分身 App 的启动组件（分身的硬要求）**



```
adb -s {serial} shell cmd package resolve-activity --brief --components --user 128 com.gof.china
# 输出形如：com.gof.china/com.unity3d.player.DDUnityLaunchActivity
```

> MagicOS 实测：必须带 
>
> `--brief --components`
>
> ，单独 
>
> `--brief`
>
>  配 
>
> `--user`
>
>  会返回空，
> 导致误报 "未找到应用"。部分设备不接受 
>
> `--user=N`
>
>  等号形式，必须 
>
> `--user N`
>
>  空格形式
> （失败时再拿等号形式兜底一次）。

**步骤 3：创建虚拟显示器并实时解析 displayId**



```
scrcpy -s {serial} --new-display=1920x1080 --window-width=1920 --window-height=1080 --window-title=无尽冬日 - Magic
```



* scrcpy 启动后约 1\~2 秒，其 **stdout 会出现一行&#x20;**`[server] INFO: New display: 1920x1080 (id=2)`（scrcpy 4.x 实测格式；旧版 3.x 为 `displayId: N`）；

* 调用方必须**阻塞读取 stdout，等这行出现**才算虚拟显示器创建成功（设 25 秒超时）；

* **displayId 每次启动都动态变化，必须实时解析，绝不能硬编码**；

* 解析失败 / 超时 → 终止 scrcpy 进程并报错，不要继续后续步骤。

**步骤 4：把分身 App 启动到该虚拟显示器**



```
adb -s {serial} shell am start-activity --user 128 --display 2 -n com.gof.china/com.unity3d.player.DDUnityLaunchActivity
```



* 检查输出：若含 `Error` / `Exception` / `Warning: Activity not started` 视为失败，

  可改用等号形式 `--user=128 --display=2` 重试一次；

* 成功后 App 渲染到 scrcpy 窗口对应的虚拟显示器，**手机主屏不受影响**；

* 关闭 scrcpy 窗口 → 虚拟显示器自动移除，手机恢复原状。

### 8.3 关键坑位清单（全部实测踩过）



| # | 坑                                    | 现象                                            | 正确做法                               |
| - | ------------------------------------ | --------------------------------------------- | ---------------------------------- |
| 1 | `am start` 没传 `--display`            | App 在手机**前台**启动，scrcpy 显示主屏（"映射模式"），虚拟显示器窗口空白 | 必须 `--display <id>`                |
| 2 | `--user` 不传 / 传错                     | 分身场景报错或启动到机主                                  | 必须显式 `--user 128`；注意空格 / 等号形式兼容    |
| 3 | `resolve-activity` 不带 `--components` | 返回空 → 误报 "未找到应用"                              | 必须 `--brief --components --user N` |
| 4 | displayId 硬编码                        | 换一次连接 / 重启后对不上，窗口空白                           | 每次从 scrcpy stdout 实时解析             |
| 5 | 先 `am start` 再建 VD（顺序反）              | App 已在前台，VD 创建后是空的                            | 必须先建 VD 拿到 displayId，再 am start    |
| 6 | 以 VD 创建 "进程启动" 为准                    | 以为启动了就成功，实际显示器 ID 还没出来                    | 以 stdout 出现 `New display: ... (id=N)`（4.x）行为准       |
| 7 | VD 分辨率与物理屏不一致                        | App 按 VD 分辨率渲染（可接受）；**荣耀设备会污染手势热区**           | 见第 6 节「修复手机手势」按钮                   |

### 8.4 通用实现伪代码（语言无关）



```
serial = "adb-XXXX._adb-tls-connect._tcp"
uid = 128
pkg = "com.gof.china"
res = "1920x1080"

# 1) 解析组件
component = adb("resolve-activity --brief --components --user {uid} {pkg}")
                .lines.first(含 '/' 且非 "No activity")

# 2) spawn scrcpy（不阻塞主流程），后台线程读 stdout
proc = spawn("scrcpy", ["-s", serial, "--new-display=" + res,
                       "--window-width=1920", "--window-height=1080", ...])
display_id = null
for line in proc.stdout (阻塞读取, 25s 超时):
    if "New display:" in line and "(id=" in line:  # scrcpy 4.x 格式
        display_id = int(line.split("(id=")[1].rstrip(")"))
        break
    if "displayId:" in line:  # 旧版 3.x 格式
        display_id = int(line.split("displayId:")[1].strip())
        break
if display_id is null:
    proc.kill()
    throw "创建虚拟显示器失败/超时"

# 3) 启动分身到虚拟显示器
out = adb("am start-activity --user {uid} --display {display_id} -n {component}")
if "Error" in out or "Exception" in out or "Warning: Activity not started" in out:
    retry("am start-activity --user={uid} --display={display_id} -n {component}")

# 4) 用户关闭 scrcpy 窗口 → 虚拟显示器自动移除
```

### 8.5 在 gscrcpy 中的落地位置（参考实现）



| 模块              | 职责                                                                                                                                                  |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/adb.rs`    | `resolve_activity()`（--brief --components --user N，空格优先等号兜底）；`start_app_for_user_on_display()`（am start-activity --user N --display D，失败自动兜底重试）     |
| `src/scrcpy.rs` | `build_clone_args()` 生成 `--new-display=WxH` 参数；`launch_with_new_display()` → `spawn_with_display()`（后台线程读 stdout 解析虚拟显示器 ID：4.x 输出 `New display: ... (id=N)`、旧版 `displayId: N`，两种兼容；25 秒超时，超时 kill 进程） |
| `src/app.rs`    | `launch_scrcpy()` 的 clone\_user 分支按上述四步顺序执行，任一步失败带真实原因提示                                                                                            |



***

## 9. 排障速查



```
# 查看设备
adb devices -l

# 物理分辨率（取 Physical size）
adb -s {serial} shell wm size

# 品牌判断（决定是否需要手势热区修复）
adb -s {serial} shell getprop ro.product.brand
adb -s {serial} shell getprop ro.product.manufacturer

# 分身用户列表
adb -s {serial} shell pm list users

# 查看手势热区（GestureNav）注册状态 —— 排查手势失效的关键
adb -s {serial} shell dumpsys input | findstr /i "GestureNav"

# 列出分身用户三方应用
adb -s {serial} shell pm list packages -3 --user 128

# 列出有桌面入口的应用（组件形式）
adb -s {serial} shell cmd package query-activities --brief --components --user 128 -a android.intent.action.MAIN -c android.intent.category.LAUNCHER
```



***

## 10. 经验总结（一句话版）



1. 荣耀 MagicOS 的**手势热区会被虚拟显示器分辨率污染**，物理分辨率 VD 覆盖一次即固化，无需重启手机 → gscrcpy 已提供「修复手机手势」手动按钮。

2. 分身场景 `resolve-activity` **必须** `--brief --components --user N`，否则误报 "未找到应用"。

3. 画面只占一部分 = 分辨率比例不一致，留空直接镜像或点「用设备分辨率」。

4. 无线配对失败先看真实日志，配对码手动配对是二维码的可靠兜底。

5. 同一手机显示两个条目 = mDNS 串号 + ip:port 重复，按串号去重、默认屏蔽 IP。