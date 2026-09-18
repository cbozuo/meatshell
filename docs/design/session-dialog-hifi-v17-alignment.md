# 新建会话对话框 · 按高保真 v17 对齐落地

参照稿：`.workbuddy/prototypes/new-session-dialog-alt-dropdown-2026-09-17.html`
（原型角标：**v17 · 2026-09-17 · 头部单行化 61→49px**）

本次以该原型为唯一准绳，把代码里不符合的地方整体改掉。核心问题不是图标，而是
**协议选择方式与区块顺序仍是旧结构**。

---

## 1 结构性改动

| # | 项目 | 旧实现 | 高保真 v17 / 现实现 |
|---|------|--------|---------------------|
| 1 | 协议选择 | **协议标签轨道**（固定 4 个槽位 `SegOption`）+「更多」按钮 + 卡片内 overlay 面板；带固定/取消/拖拽排序/重置 | **字段级「连接协议」下拉**（`ProtoSelect`）：按钮 = 图标 15px + 名称 + 「即将/预览」徽标 + ▾；弹层按分类分组、当前项打勾 |
| 2 | 区块顺序 | 标签轨道 → 主机/端口 → 名称/分组/备注 → 用户名/密码 → 认证 → 高级 | **① 标识（名称+分组 / 备注）→ ② 连接到哪 → ③ 认证 → ④ 高级**；标识区提到顶层，所有协议共用 |
| 3 | 连接行布局 | 主机/端口 单独一行，协议在标签轨道里 | **协议 + 主机/串口 + 端口/波特率 同一行**（列宽 1.25fr / 1.5fr / 0.8fr） |
| 4 | 串口四参数 | 数据位/停止位/校验位/流控 = `SegOption` 分段按钮，两个 `VerticalLayout` | **四个下拉框并排一行**（`ComboBox`） |
| 5 | RDP/VNC 分支 | 预留卡 → 主机/端口 → 名称/分组/备注 → 用户名/密码 | **标识（共用）→ 预留卡 + 连接行 → 用户名/密码** |
| 6 | 高级区 | Serial 只渲染内容不渲染开关（能切出「有字段无按钮」的卡死态） | **所有协议统一「按钮 + 折叠容器」** |
| 7 | 区块分隔 | 无分隔线，仅 20px 间距 | **区块间 1px 分隔线 + 14px 间距** |

## 2 头部

| 项目 | 旧 | 新 |
|------|----|----|
| 头部高度 | 66px | **49px** |
| 协议字形块 | 34×34，圆角 10，图标 18px | **28×28，圆角 8，图标 15px** |
| 标题 / 副标题 | 上下两行（占 36px，是偏高的主因） | **同一行并排**，标题 flex 0、副标题 flex 1 可省略号截断 |
| 关闭按钮 | 32×32，圆角 4 | **28×28，圆角 7** |

## 3 控件与手感

| 项目 | 旧 | 新 |
|------|----|----|
| 输入框高度 | 32px | **38px**（`LabeledInput` 新增 `input-height` 属性，其它对话框仍 32px） |
| 聚焦态 | 1px 描边 + **左侧 2px 强调色竖条**（`accent-rail`） | **只留 1px 强调色描边**；竖条全部移除（原型：#focus-uniform 指出竖条让左边看起来比右边粗） |
| 页脚按钮 | 30px | **38px** |
| 协议图标 | 上一轮挑了 router / memory / computer，与原型造型不符 | 按原型 SVG 造型逐码位对齐：ssh→`terminal`、serial→`memory`、telnet→`web_asset`、rdp/vnc→`desktop_windows` |

## 4 新增 / 删除的代码

**新增**
- `ui/session_dialog.slint` → `component ProtoItem`（列表项）、`component ProtoSelect`（下拉选择器 + 弹层）
- `SessionDialog.parity-text / parity-from-text / flow-text / flow-from-text`：`ComboBox` 的 model 只能是 `[string]`，用它做显示文本 ↔ 协议值双向映射
- `ui/widgets.slint` → `LabeledInput.input-height`（默认 32px）

**删除（标签轨道整条链路，前后端一并清）**
- UI：`component PinRow`、`component OtherProtoRow`；SessionDialog 的 `pinned-protocols` / `preview-kind` 之外的 pin 成员、`more-open` / `more-count` / `pin-drag-*` / `pin-at` / `pin-is` / `is-pinned` / `proto-tab-badge` / `pin-full` / `has-unpinned-real` / `on-pin-drag-*` / `pin-shift` / `emit-pins` / `unpin` / `pin-add` / `swap-pins`；「更多」overlay 面板与 dismiss 层
- `ui/app.slint`：`dialog-pinned` 属性、`pinned-changed` / `reset-pinned` 回调及其绑定与转发
- `src/app.rs`：3 处 `set_dialog_pinned(...)`、`on_pinned_changed` / `on_reset_pinned` 两个 handler
- `src/app/session_models.rs`：`pinned_model` / `pins_from_model`
- `src/config/impls/config.rs`：`pinned_protocols()` / `set_pinned_protocols()` / `normalize_pinned_protocols()` + 3 个常量
- `src/config/struct/config_file.rs`：`pinned_protocols` 字段（无 `deny_unknown_fields`，旧配置里的该键会被静默忽略）

## 5 保留

`preview-kind` / `is-preview` / `start-preview` —— RDP / VNC 尚未实现，选到它们仍进入「预览」态：只渲染布局、`创建` 按钮禁用。

---

## 6 追加：按运行截图修正的偏差（R20）

上面 1–4 是静态比对原型源码得到的改动。但**跑起来仍有明显偏差** —— 用户截图暴露了
「代码结构对了、界面还不对」的部分。以下 6 处按实际渲染修正：

| # | 现象 | 原因 | 修法 |
|---|------|------|------|
| 1 | 全屏时弹窗撑满整屏，内容只占上半截、下面大片留白；首字段被挤出可视区 | 高度取视口的 78% | 改为**内容自适应**：`min(body.preferred-height + hdr.height + foot.height + 2px, parent.height - 32px)` |
| 2 | 改高度后编译报绑定循环 | `dlg-card.height → layout-cache → foot.height` 互相依赖 | 给 `foot` **写死 `height: 62px`**（12 + 38 + 12），卡片再引用它 |
| 3 | 头部「新建会话 + 配置 SSH 连接」跑到弹窗中央，与左侧字形块脱开 | `HorizontalLayout` 的 `alignment: center` 是**主轴**对齐 | 去掉 `alignment`；交叉轴居中由布局自身负责 |
| 4 | 认证区顺序为「用户名 → 认证方式 → 密码」，且各占一行 | — | 改为「认证方式 → 用户名 + 密码（同行）」；私钥模式下用户名单独一行在前 |
| 5 | 名称 / 分组各占一行 | — | 名称 + 分组**同行**；`GroupCombo` 输入框 32 → 38px 与 `LabeledInput` 对齐 |
| 6 | 「高级（字符集、代理、隧道）」 | 旧文案 | 改为 `显示高级选项` / `隐藏高级选项` |

### 翻译

界面上出现英文 `Connection protocol` —— 因为 `@tr("...")` 的 msgid 没进
`lang/{zh,en}/LC_MESSAGES/meatshell.po`，缺条目时会**回退显示原文**，代码里看不出来。
本轮补齐 7 条：`Connection protocol` / `Remote login` / `Remote desktop` /
`Odd` / `Even` / `Show advanced options` / `Hide advanced options`。

### 验收方式

静态读代码不足以验收 UI。本轮改为**本机跑起来截窗口**对比：
Python（隔离 venv）+ pillow 的 `ImageGrab` 配合 `ctypes` 的 `EnumWindows` /
`GetWindowRect` / `SetForegroundWindow`，只截目标窗口矩形。
（本机 PowerShell 的 `Add-Type` 被安全策略禁止，不能用 `System.Drawing` 截屏。）
