//! (#close-behavior) 系统托盘:主窗口「最小化到托盘」时保持后台运行。
//!
//! 设计要点:
//! - **零新依赖**。托盘用 `windows` crate 的 `Shell_NotifyIconW` 实现;该 crate
//!   本就在 `[target.'cfg(windows)'.dependencies]` 里,本次只补了三个 feature
//!   (`Win32_UI_WindowsAndMessaging` / `Win32_System_LibraryLoader` /
//!   `Win32_Graphics_Gdi`),不引入新的第三方 crate。
//! - 一个不可见的消息窗口负责收托盘回调;它挂在主线程上,由 winit 已有的事件
//!   循环派发消息,因此**不另开消息循环**。
//! - 右键不再走 Win32 `TrackPopupMenu`(原生菜单画不出每项图标、也不能 hug 最长
//!   一行)。改为把光标坐标作为 `TrayAction::OpenMenu` 交回 `app.rs`,由自绘
//!   Slint `TrayMenuWindow` 弹出。左键仍直接 `Show`。退出经回调交回 `app.rs`,
//!   复用既有的 `win-close` 收尾路径(保存布局 → 断开全部会话 → 退出事件循环),
//!   不另起一套拆除逻辑。
//! - 非 Windows 平台整体 no-op:`Tray::ensure` 仍可调用,只是不出现托盘项。
//!   (macOS 的 `NSStatusItem`、Linux 的 `StatusNotifierItem` 不在本次范围。)

/// 托盘菜单动作。由 `app.rs` 决定怎么响应。
#[derive(Clone, Copy)]
pub(crate) enum TrayAction {
    /// 用户要回主窗口(左键单击,或菜单「显示」)
    Show,
    /// 右键:在光标处打开自绘菜单。坐标是屏幕物理像素(`GetCursorPos`)。
    OpenMenu { x: i32, y: i32 },
}

/// 托盘宿主回调。`app.rs` 传入,内部只负责把动作转成 `TrayAction`。
pub(crate) type TraySink = Box<dyn Fn(TrayAction)>;

/// 托盘句柄。持有期间托盘项存在;`Drop` 时移除图标并销毁消息窗口。
pub(crate) struct Tray {
    #[cfg(windows)]
    _inner: win::TrayWin,
}

impl Tray {
    /// 创建托盘(重复调用只会替换回调,不会重复添加图标)。
    ///
    /// 应在主线程、Slint 事件循环启动后调用。
    pub(crate) fn ensure(sink: TraySink) -> Self {
        #[cfg(windows)]
        {
            Self {
                _inner: win::ensure(sink),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = sink;
            Self {}
        }
    }

    /// (#close-exit-fix 2026-09-19) 托盘图标是否可用:供「最小化到托盘」
    /// 失败兜底(NIM_ADD 失败时不能 hide 窗口,否则无窗无托盘 = 僵尸进程)。
    #[allow(unused)]
    pub(crate) fn icon_ok() -> bool {
        #[cfg(windows)]
        {
            win::icon_ok()
        }
        #[cfg(not(windows))]
        {
            true
        }
    }

    /// (#tray-icon-vanish 2026-09-21) NIM_MODIFY 心跳:强制 Explorer 重画
    /// 托盘图标。自绘弹层抢前台后图标偶发不重画(用户截图),关闭弹层时调用。
    #[allow(unused)]
    pub(crate) fn refresh_icon() {
        #[cfg(windows)]
        win::refresh_icon();
    }

    /// (#tray-persist 2026-09-21) 托盘消息宿主 HWND(0 = 未创建)。弹层/关于窗
    /// 设为其 owner → 属主窗口不进任务栏、不产生独立任务栏按钮(根治闪现)。
    #[allow(unused)]
    pub(crate) fn host_hwnd() -> isize {
        #[cfg(windows)]
        {
            win::host_hwnd()
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// (#tray-flyout-r8) 菜单打开期间装低级鼠标钩子(点菜单外收起)。
    #[allow(unused)]
    pub(crate) fn install_menu_hook(menu_hwnd: isize) {
        #[cfg(windows)]
        win::install_menu_hook(menu_hwnd);
    }

    /// (#tray-flyout-r8) 卸载低级鼠标钩子(菜单关闭时调用)。
    #[allow(unused)]
    pub(crate) fn remove_menu_hook() {
        #[cfg(windows)]
        win::remove_menu_hook();
    }
}

// ======================================================================
// Windows 实现
// ======================================================================
#[cfg(windows)]
mod win {
    use super::{TrayAction, TraySink};
    use std::cell::{Cell, RefCell};
    use std::mem::size_of;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos,
        GetSystemMetrics, GetWindowRect, HHOOK, LoadImageW, MSLLHOOKSTRUCT, RegisterClassW,
        RegisterWindowMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL,
        CW_USEDEFAULT, HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, LR_DEFAULTSIZE,
        LR_SHARED, SM_CXSMICON, SM_CYSMICON, WM_APP, WM_CONTEXTMENU, WM_DESTROY,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
        WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
    };
    /// 托盘回调消息(自定义区起始值);winit 的消息循环会派发到 wnd_proc。
    const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
    /// `NOTIFYICONDATAW.szTip` 声明长度。
    const TIP_LEN: usize = 128;

    thread_local! {
        /// 单窗口单主线程 → 每线程一个托盘宿主。
        static SINK: RefCell<Option<TraySink>> = const { RefCell::new(None) };
        /// 宿主消息窗口;空表示尚未创建。
        static HOST_HWND: RefCell<isize> = const { RefCell::new(0) };
        /// (#close-exit-fix 2026-09-19) 最近一次 NIM_ADD 是否成功 —— 供
        /// minimize_to_tray 决定"真进托盘"还是"回落弹确认卡"。
        static TRAY_OK: Cell<bool> = const { Cell::new(false) };
        /// (#tray-icon-vanish 2026-09-21) ADD 成功的 HICON 句柄,供 refresh_icon
        /// 复用 —— MODIFY 重申字段时不能重新 LoadImage(可能拿到新句柄且浪费 GDI)。
        static LAST_ICON: Cell<isize> = const { Cell::new(0) };
        /// (#tray-flyout-r8) 菜单打开期间的低级鼠标钩子与菜单窗口 HWND
        /// (0 = 未装钩)——点击菜单矩形外时收起菜单。
        static MENU_HOOK: Cell<isize> = const { Cell::new(0) };
        static MENU_HWND: Cell<isize> = const { Cell::new(0) };
    }

    /// 托盘图标当前是否可用(最近一次 ADD 的结果;宿主未创建 = false)。
    pub(super) fn icon_ok() -> bool {
        if HOST_HWND.with(|h| *h.borrow()) == 0 {
            return false;
        }
        TRAY_OK.with(|o| o.get())
    }

    /// (#tray-persist 2026-09-21) 托盘消息宿主窗口的 HWND(0 = 未创建)。
    /// 供弹层/关于窗设为其 owner(属主窗口不进任务栏、随属主管理)。
    pub(super) fn host_hwnd() -> isize {
        HOST_HWND.with(|h| *h.borrow())
    }

    fn emit(action: TrayAction) {
        SINK.with(|s| {
            if let Some(sink) = s.borrow().as_ref() {
                sink(action);
            }
        });
    }

    pub(super) fn ensure(sink: TraySink) -> TrayWin {
        // 已有宿主:只换回调,不重复 ADD。
        let already = HOST_HWND.with(|h| *h.borrow());
        if already != 0 {
            SINK.with(|s| *s.borrow_mut() = Some(sink));
            return TrayWin {
                hwnd: HWND(already as *mut _),
            };
        }

        SINK.with(|s| *s.borrow_mut() = Some(sink));

        unsafe {
            let hinstance = GetModuleHandleW(None).expect("GetModuleHandleW failed");
            let class = w!("MeatshellTrayHost");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: HINSTANCE(hinstance.0),
                lpszClassName: class,
                ..Default::default()
            };
            // 类已注册时返回 0,可忽略。
            let _ = RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                w!("meatshell tray"),
                WS_OVERLAPPED,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                HINSTANCE(hinstance.0),
                None,
            )
            .expect("CreateWindowExW(tray host) failed");

            HOST_HWND.with(|h| *h.borrow_mut() = hwnd.0 as isize);
            // (#close-exit-fix 2026-09-19) NIM_ADD 结果要上报:失败时
            // minimize_to_tray 不再盲目 hide 窗口(否则无窗无托盘 = 僵尸进程)。
            let ok = add_icon(hwnd);
            TRAY_OK.with(|o| o.set(ok));
            tracing::info!(hwnd = hwnd.0 as isize, ok, "tray: host window created");
            TrayWin { hwnd }
        }
    }

    /// 用系统默认图标 ADD 托盘项。返回 NIM_ADD 是否成功 —— 失败意味着
    /// 「最小化到托盘」没有可唤回的入口,调用方必须换路径而不是隐藏窗口。
    unsafe fn add_icon(hwnd: HWND) -> bool {
        // (#tray-icon-real 2026-09-19) 优先加载 **exe 内嵌的软件图标**
        // （build.rs 的 winresource 以资源 ID 1 嵌入 assets/meatshell.ico）。
        // 托盘按小图标渲染,按 SM_CXSMICON/SM_CYSMICON 尺寸加载最清晰。
        // 09-18 的教训仍成立:加载资源图标必须传 exe 模块句柄;系统预定义
        // 图标（IDI_APPLICATION）必须传 NULL —— 两者分开,失败时回落占位,
        // 保证托盘总有图标。
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let cx = GetSystemMetrics(SM_CXSMICON).max(16);
        let cy = GetSystemMetrics(SM_CYSMICON).max(16);
        let icon = LoadImageW(
            HINSTANCE(hinstance.0),
            PCWSTR(1usize as *const u16), // MAKEINTRESOURCEW(1): RT_GROUP_ICON ID 1
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR,
        )
        .or_else(|e| {
            tracing::warn!("tray: exe icon load failed ({e}), falling back to IDI_APPLICATION");
            LoadImageW(
                HINSTANCE(std::ptr::null_mut()),
                IDI_APPLICATION,
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED,
            )
        })
        .unwrap_or_else(|e| {
            tracing::warn!("tray: LoadImageW(IDI_APPLICATION) failed: {e}");
            Default::default()
        });

        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY_CALLBACK,
            hIcon: HICON(icon.0),
            ..Default::default()
        };
        let tip: Vec<u16> = "meatshell"
            .encode_utf16()
            .chain(std::iter::once(0))
            .take(TIP_LEN)
            .collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);

        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            let err = windows::core::Error::from_win32();
            tracing::warn!("tray: Shell_NotifyIconW(NIM_ADD) failed: {err}");
            return false;
        }
        // (#tray-icon-vanish 2026-09-21) 记住已注册的图标句柄,refresh_icon 用。
        LAST_ICON.with(|i| i.set(icon.0 as isize));
        // (#tray-icon-vanish 2026-09-21) 撤回 09-19 引入的 NOTIFYICON_VERSION_4:
        // 它当时从未被验证过(旧代码 uVersion 恒 0,SETVERSION 是空操作),且
        // 社区有 v4 下图标异常消失的报告;v0 是绝大多数托盘应用走的路径(用户
        // 对比"其它软件托盘不会消失")。v0 下回调 lParam 依旧是鼠标事件,
        // wnd_proc 的 WM_LBUTTONUP/WM_RBUTTONUP 匹配不受影响。
        // NIM_MODIFY 保留:ADD 成功但 Explorer 偶发不渲染图标的踢一下兜底。
        let mf = Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool();
        tracing::info!(mf, "tray: icon added (NIM_ADD ok + refresh)");
        true
    }

    /// (#tray-show-fix 2026-09-19) Explorer 重启(崩溃/用户杀进程)后任务栏
    /// 重建,之前注册的所有托盘图标都会消失;唯一恢复手段是监听系统广播
    /// 消息 "TaskbarCreated" 并重新 ADD。消息 id 每次开机不同,须运行时注册。
    fn taskbar_created_msg() -> u32 {
        static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        *ID.get_or_init(|| unsafe {
            RegisterWindowMessageW(w!("TaskbarCreated"))
        })
    }

    /// (#tray-icon-vanish 2026-09-21) 重申托盘图标字段(NIM_MODIFY),强制
    /// Explorer 重画图标。自绘弹层抢前台会让 Explorer 的托盘悬停/溢出层
    /// 收起,图标偶发停留在"未重画"态 —— 每次关闭弹层后 MODIFY 一次兜底。
    /// 不带 NIF_TIP(避免把 tooltip 清空,又免重新拼 UTF-16)。
    pub(super) fn refresh_icon() {
        let hwnd = HOST_HWND.with(|h| *h.borrow());
        let hicon = LAST_ICON.with(|i| i.get());
        if hwnd == 0 || hicon == 0 {
            return;
        }
        unsafe {
            let nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: HWND(hwnd as *mut _),
                uID: 1,
                uFlags: NIF_MESSAGE | NIF_ICON,
                uCallbackMessage: WM_TRAY_CALLBACK,
                hIcon: HICON(hicon as *mut _),
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// 移除托盘图标。
    unsafe fn remove_icon(hwnd: HWND) {
        let nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAY_CALLBACK => {
                // (#tray-icon-vanish) v0(撤回 v4 后):lParam 即鼠标事件。
                let evt = (lparam.0 as u32) & 0xffff;
                // (#tray-show-fix 2026-09-19) 记录事件用于回归验证。
                tracing::info!(evt, "tray: callback event");
                match evt {
                    WM_LBUTTONUP => emit(TrayAction::Show),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            msg if msg == taskbar_created_msg() => {
                // (#tray-show-fix 2026-09-19) Explorer 重启后任务栏重建:
                // 重新注册图标(同 hWnd+uID 的 ADD 是幂等更新)。
                tracing::info!("tray: TaskbarCreated — re-adding icon");
                unsafe { add_icon(hwnd) };
                LRESULT(0)
            }
            WM_DESTROY => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// 右键:把光标坐标交给 `app.rs` 弹出自绘菜单。
    ///
    /// 不再调用 `TrackPopupMenu` —— 原生菜单画不出每项图标,也不能 hug 内容宽度。
    unsafe fn show_menu(_hwnd: HWND) {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        emit(TrayAction::OpenMenu { x: pt.x, y: pt.y });
    }

    /// (#tray-flyout-r8) 菜单打开期间装**低级鼠标钩子**:点击菜单窗口矩形之外
    /// 时收起菜单(NOACTIVATE 窗口收不到焦点变化,"点外部收起"的标准实现,
    /// 与原生菜单一致;点击本身穿透不吞)。装/卸必须配对(install 于弹层 show、
    /// remove 于弹层 hide),钩子回调要求安装线程持续泵消息——主线程 winit
    /// 事件循环满足。
    pub(super) fn install_menu_hook(menu_hwnd: isize) {
        MENU_HWND.set(menu_hwnd);
        unsafe {
            let Ok(hook) = SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(menu_hook_proc),
                HINSTANCE(std::ptr::null_mut()),
                0,
            ) else {
                return;
            };
            MENU_HOOK.set(hook.0 as isize);
        }
    }

    pub(super) fn remove_menu_hook() {
        let hook = MENU_HOOK.replace(0);
        MENU_HWND.set(0);
        if hook != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
            }
        }
    }

    /// (#tray-flyout-r8) 低级鼠标钩子过程:左/右键按下时判定点击点是否落在
    /// 菜单窗口矩形外 —— 外部则隐藏弹层(经 app.rs 的隐藏链路,含图标心跳与
    /// 延迟销毁),点击本身照常穿透给系统(不吞,点哪哪生效)。
    unsafe extern "system" fn menu_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let msg = wparam.0 as u32;
            if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN {
                let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                let menu = MENU_HWND.get();
                if menu != 0 {
                    let mut rect = std::mem::zeroed();
                    if GetWindowRect(HWND(menu as *mut _), &mut rect).is_ok() {
                        let inside = info.pt.x >= rect.left
                            && info.pt.x < rect.right
                            && info.pt.y >= rect.top
                            && info.pt.y < rect.bottom;
                        if !inside {
                            super::super::hide_tray_flyout();
                        }
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// 托盘生命周期句柄。`Drop` 负责移除图标并销毁消息窗口。
    pub(super) struct TrayWin {
        hwnd: HWND,
    }

    impl Drop for TrayWin {
        fn drop(&mut self) {
            // (#close-exit-fix 2026-09-19) 这个 drop 若在「最小化到托盘」后
            // 立刻出现,说明事件循环被意外退出(hide 最后窗口),托盘刚建好
            // 就被收尾链拆掉 —— 用户看到"托盘里没有图标"。
            tracing::info!("tray: TrayWin dropped (icon removed, host destroyed)");
            unsafe { remove_icon(self.hwnd) };
            HOST_HWND.with(|h| *h.borrow_mut() = 0);
            // (#tray-flyout-r4-r2) try_borrow_mut:若 Drop 发生在 emit 持有
            // SINK 借用的回调栈内(直接 take+drop 的写法会进到这里 → RefCell
            // 冲突 panic,panic=abort 下进程直接消失),静默跳过——槽位残留的
            // 旧闭包会在下次 ensure 时被覆盖,无害。常规路径已改为延迟出栈。
            SINK.with(|s| {
                if let Ok(mut slot) = s.try_borrow_mut() {
                    *slot = None;
                }
            });
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
