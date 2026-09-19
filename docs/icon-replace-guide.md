# 图标 / Logo 替换指南

本文说明 meatshell 各处图标的位置、格式与尺寸要求。替换后**必须重新编译**才生效（图标都是编译期打进产物或由 exe 资源承载的，没有运行时加载路径）。

建议：准备一张 **1024×1024 的 PNG 方形源图**（透明背景、内容居中、四周留 5~10% 空白），由它统一导出下面所有格式。

## 总览

| 文件 | 用在哪里 | 格式 / 尺寸 | 谁来加载 |
|---|---|---|---|
| `assets/meatshell.ico` | ① exe 文件图标（资源管理器/快捷方式/任务栏）② **系统托盘图标** | Windows ICO，多尺寸合集 | exe 资源（资源 ID 1）→ 托盘代码按小图标尺寸加载 |
| `assets/icon.png` | 应用内窗口标题栏左上角 logo（主窗口/进程/系统信息/编辑器等所有窗口） | PNG 256×256 RGBA | Slint `@image-url`，编译期打包 |
| `assets/icon@512.png` | Linux：任务栏/窗口图标 + 系统应用图标（deb 安装） | PNG 512×512 RGBA | `include_bytes!` + `.desktop` 安装脚本 |
| `assets/Info.plist` 指向的 icns | macOS Dock/访达图标 | icns（macOS 打包时生成） | macOS 应用包 |

## 1. Windows：`assets/meatshell.ico`（托盘图标 + exe 图标）

**一个文件管两处**：`build.rs` 里 `winresource::WindowsResource::set_icon()` 把它嵌入 exe 资源段（资源 ID **1**）；`src/app/tray.rs` 的 `add_icon()` 用 `LoadImageW(exe句柄, MAKEINTRESOURCE(1), IMAGE_ICON, SM_CXSMICON, SM_CYSMICON)` 加载它作为托盘图标。

**格式要求**：
- ICO 容器，**必须包含这些尺寸层**：`16×16`、`24×24`、`32×32`、`48×48`、`64×64`、`128×128`、`256×256`（层内用 PNG 压缩即可，256 层必须是 PNG）
- **托盘图标实际读取的是 16×16 / 32×32 两层**（100% DPI 用 16px，150%/200% DPI 用 32px；Windows 按系统 DPI 自动选层）——这两层必须清晰，不要只塞一张 256 大图让系统缩放
- RGBA 全彩 + 透明背景，圆角/内容比例参考现有 `assets/meatshell.ico`
- 单层没有透明度托盘上会显示白底方块

**替换方法**（任选）：
```sh
# ImageMagick（推荐，一条命令生成全尺寸层）
magick 源图-1024.png -define icon:auto-resize=256,128,64,48,32,24,16 assets/meatshell.ico
```
或在线工具（icoconvert / convertio 等）上传 1024 源图勾选全部尺寸导出。

**替换后**：重新 `cargo build`（build.rs 已声明 `rerun-if-changed=assets/meatshell.ico`，会自动重嵌资源）。验证托盘图标：任务栏时钟左侧（已提升显示时）或 ^ 溢出弹窗里；资源管理器里看 exe 图标（有系统图标缓存，必要时改名 exe 或重启资源管理器刷新）。

## 2. 应用内窗口 Logo：`assets/icon.png`

主窗口、进程窗口、系统信息窗口、编辑器窗口标题栏的左上角小 logo（Slint 里 6 处 `@image-url("../assets/icon.png")` 引用，全部指向这一个文件）。

**格式要求**：PNG，**256×256**，RGBA 透明背景。显示尺寸很小（约 20~28px 逻辑像素），图案务必简单、居中。

**替换后**：重新 `cargo build`（`@image-url` 是编译期打包进二进制的，不是运行时读文件）。

## 3. Linux：`assets/icon@512.png`

三处使用：
- 窗口/任务栏图标：`src/app/window.rs` 的 `set_window_icon()`（`include_bytes!("../../assets/icon@512.png")`，仅 Linux 编译生效）
- deb 包安装：`scripts/build-deb-debian13.sh` 装到 `/usr/share/icons/hicolor/512x512/apps/meatshell.png`
- tar 包安装：`assets/install-linux.sh` 的 `ICON_SRC`

**格式要求**：PNG，**512×512**，RGBA 透明背景。文件名保持 `icon@512.png` 不变（代码/脚本按名字引用）。

**替换后**：重新编译；deb/tar 安装包重新打包。

## 4. macOS

`assets/Info.plist` 声明 `CFBundleIconFile=meatshell`，需要应用包内的 `meatshell.icns`（当前 assets 下没有，构建 macOS 包时从 1024 源图生成）：
```sh
mkdir icon.iconset
for s in 16 32 64 128 256 512 1024; do
  magick 源图-1024.png -resize ${s}x${s} icon.iconset/icon_${s}x${s}.png
done
# 再补 @2x 命名后
iconutil -c icns icon.iconset -o meatshell.icns
```

## 5. 检查清单

替换完所有文件后：

- [ ] `assets/meatshell.ico` 含 16/24/32/48/64/128/256 七个尺寸层
- [ ] `assets/icon.png` 256×256、`assets/icon@512.png` 512×512
- [ ] 所有文件名未改动（代码与脚本按文件名引用）
- [ ] `cargo build` 全量重编通过
- [ ] Windows 实测：托盘图标（时钟左侧或 ^ 溢出弹窗）显示新 logo，左键单击唤回正常
- [ ] Linux/macOS 如有打包需求，重新出包验证

## 附：本项目托盘图标的加载细节（供排查）

- 托盘图标加载点：`src/app/tray.rs` → `add_icon()`，优先资源 ID 1（软件图标），加载失败回落系统 `IDI_APPLICATION` 占位（表现为白色空白窗口图案——看到它说明 exe 资源里没有合法图标层）
- 托盘按 **小图标尺寸**（`SM_CXSMICON`×`SM_CYSMICON`）取层，这是 ico 里 16/32 层重要的原因
- Win11 已知顽疾：`NIM_ADD` 报成功但图标可能不渲染，代码里已在 ADD 后用 `NIM_MODIFY` 踢一次强制刷新（`#tray-show-fix`）；Explorer 重启后靠 `TaskbarCreated` 消息重注册（`#tray-icon-real`）
- 系统对每个 exe 路径的记忆：`HKCU\Control Panel\NotifyIconSettings\<hash>`（`IsPromoted=1` 显示在主任务栏，缺失则在 ^ 溢出区）
