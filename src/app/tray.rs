//! (#close-behavior) 系统托盘:主窗口「最小化到托盘」时保持后台运行。
//!
//! 设计要点:
//! - **零新依赖**。托盘用 `windows` crate 的 `Shell_NotifyIconW` 实现;该 crate
//!   本就在 `[target.'cfg(windows)'.dependencies]` 里,本次只补了三个 feature
//!   (`Win32_UI_WindowsAndMessaging` / `Win32_System_LibraryLoader` /
//!   `Win32_Graphics_Gdi`),不引入新的第三方 crate。
//! - 一个不可见的消息窗口负责收托盘回调;它挂在主线程上,由 winit 已有的事件
//!   循环派发消息,因此**不另开消息循环**。
//! - 右键弹 Win32 原生菜单:「显示主窗口」/「退出」。退出经回调交回 `app.rs`,
//!   复用既有的 `win-close` 收尾路径(保存布局 → 断开全部会话 → 退出事件循环),
//!   不另起一套拆除逻辑。
//! - 非 Windows 平台整体 no-op:`Tray::ensure` 仍可调用,只是不出现托盘项。
//!   (macOS 的 `NSStatusItem`、Linux 的 `StatusNotifierItem` 不在本次范围。)

/// 托盘菜单动作。由 `app.rs` 决定怎么响应。
pub(crate) enum TrayAction {
    /// 用户要回主窗口(左键单击,或菜单「显示主窗口」)
    Show,
    /// 用户要真正退出(菜单「退出」)
    Exit,
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
        NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DestroyWindow, GetCursorPos, GetSystemMetrics, LoadImageW, PostMessageW, RegisterClassW,
        RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, CW_USEDEFAULT, HICON,
        IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, LR_DEFAULTSIZE, LR_SHARED, MF_SEPARATOR,
        MF_STRING, SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON,
        WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
        WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
    };
    /// 托盘回调消息(自定义区起始值);winit 的消息循环会派发到 wnd_proc。
    const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
    /// 菜单命令 id。
    const CMD_SHOW: usize = 1;
    const CMD_EXIT: usize = 2;
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
    }

    /// 托盘图标当前是否可用(最近一次 ADD 的结果;宿主未创建 = false)。
    pub(super) fn icon_ok() -> bool {
        if HOST_HWND.with(|h| *h.borrow()) == 0 {
            return false;
        }
        TRAY_OK.with(|o| o.get())
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
        // 真正启用 v4:回调消息的 lParam 低 16 位是鼠标事件(WM_CONTEXTMENU
        // 等),wnd_proc 的匹配才有效。旧代码 uVersion 保持 0,SETVERSION 是
        // 空操作,v4 分支从未被验证。uVersion 与 uTimeout 是同一位的 union
        // (windows 0.58 的 NOTIFYICONDATAW_0),写 uVersion 侧即可。
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let sv = Shell_NotifyIconW(NIM_SETVERSION, &nid).as_bool();
        // (#tray-show-fix 2026-09-19) NIM_MODIFY 踢一下:实测(Win11) NIM_ADD
        // 报成功但 Explorer 可能不渲染图标 —— 注册表 NotifyIconSettings 有
        // 条目(IsPromoted=1)、宿主窗口活着,而主栏与溢出弹窗里都没有图标。
        // MODIFY 用同一结构重申一遍字段,强制 Explorer 刷新;失败无害。
        let mf = Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool();
        tracing::info!(sv, mf, "tray: icon added (NIM_ADD ok + refresh)");
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
                // NOTIFYICON_VERSION_4 下低 16 位是鼠标事件。
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
            WM_COMMAND => {
                match wparam.0 & 0xffff {
                    CMD_SHOW => emit(TrayAction::Show),
                    CMD_EXIT => emit(TrayAction::Exit),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// 右键弹原生菜单。
    ///
    /// `TrackPopupMenu` 自带模态循环,会把选中的 `WM_COMMAND` 直接派发到
    /// `wnd_proc`,所以这里**不需要**再跑一次消息循环(跑了反而会挂住)。
    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, CMD_SHOW, w!("显示主窗口"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出 meatshell"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Win32 的已知怪癖:弹菜单前必须先 SetForegroundWindow,否则点击
        // 菜单之外菜单不会消失(会"粘"在屏幕上)。
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );
        // 同上:让菜单可靠关闭的标准做法。
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
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
            SINK.with(|s| *s.borrow_mut() = None);
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
