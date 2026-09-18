use super::*;
// (#drag-cross-group-fix 2026-09-06) 显式导入:此前经 `use super::*` 隐式
// 继承 app.rs 的 use 项,app.rs 内不再直接使用该函数后需在此声明。
use crate::config::named_display_groups;
// (#tab-group-bar 2026-09-14) 标签底部色条要用与左侧会话行同一套"显示组"语义。
use crate::config::display_group_of;
// (#group-color 2026-09-14) 分组颜色表:组名 -> "#RRGGBB"。
use std::collections::HashMap;

/// (#group-color 2026-09-14) 解析 `"#RRGGBB"` / `"#RGB"`(前导 `#` 可省)为颜色。
/// Slint 的 `Color` **没有实现 `FromStr`**,所以这里手工解析。其他形式一律视为
/// 无效 → 该组回落"无色"。
pub(super) fn parse_hex_color(hex: &str) -> Option<slint::Color> {
    let h = hex.trim().trim_start_matches('#');
    let (r, g, b) = match h.len() {
        3 => {
            // `#abc` 简写:每位重复一次(a → 0xaa)。
            let dup = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
            (dup(0)?, dup(1)?, dup(2)?)
        }
        6 => (
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(slint::Color::from_rgb_u8(r, g, b))
}

/// (#group-color 2026-09-14) 把用户输入的 hex 归一到 `#rrggbb`(小写、带 `#`)。
/// 存储形式统一后,面板里"当前选中色"的判定就是一次字符串相等,不必反复解析。
/// 非法输入返回 `None`。
pub(super) fn normalize_hex(hex: &str) -> Option<String> {
    let c = parse_hex_color(hex)?;
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        c.red(),
        c.green(),
        c.blue()
    ))
}

/// (#tab-group-bar 2026-09-14) 标签底部色条用的组色:`(绘制色, hex)`。
///
/// **必须与左侧会话行同源** —— 复用同一张 `group_colors` 表和同一套"显示组"
/// 规则(空组名 / 保留名 → "default"),否则标签色条会和它所属的组头对不上色。
/// 唯一的例外是内建本地终端:它在列表里恒归 `system` 组(见 `build_session_rows`),
/// 若直接走 `display_group_of` 会被归到 `default`,故先用内建列表判一次身份。
///
/// hex 为空 = 该组未设色 → Slint 侧不绘制色条。
pub(super) fn tab_group_color(store: &ConfigStore, session: &Session) -> (slint::Color, String) {
    let builtin = builtin_local_sessions(store.wsl_profiles());
    let group = if builtin.iter().any(|b| b.id == session.id) {
        "system".to_string()
    } else {
        display_group_of(session)
    };
    match store
        .group_colors()
        .get(group.as_str())
        .filter(|hex| !hex.trim().is_empty())
    {
        Some(hex) => (
            parse_hex_color(hex).unwrap_or_default(),
            hex.trim().to_string(),
        ),
        None => (slint::Color::default(), String::new()),
    }
}

pub(super) fn wsl_profile_model(store: &ConfigStore) -> ModelRc<WslProfileInfo> {
    let rows = store
        .wsl_profiles()
        .iter()
        .map(|profile| WslProfileInfo {
            id: profile.id.clone().into(),
            name: profile.name.clone().into(),
            distribution: profile.distribution.clone().into(),
            directory: profile.directory.clone().into(),
        })
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

pub(super) fn parse_batch_import(text: &str) -> Vec<Session> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // splitn(5) so the last field (name) may itself contain '|'.
        let parts: Vec<&str> = line.splitn(5, '|').map(str::trim).collect();
        let host = parts.first().copied().unwrap_or("");
        // Skip blank hosts and a header row like "host|port|username|...".
        if host.is_empty() || host.eq_ignore_ascii_case("host") {
            continue;
        }
        let port = parts
            .get(1)
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|&p| p > 0)
            .unwrap_or(22);
        let user = parts
            .get(2)
            .copied()
            .filter(|s| !s.is_empty())
            .unwrap_or("root");
        let password = parts.get(3).copied().unwrap_or("");
        let name = parts
            .get(4)
            .copied()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{user}@{host}"));
        let mut sess = Session {
            name,
            host: host.to_string(),
            port,
            user: user.to_string(),
            auth: AuthMethod::Password,
            ..Session::new_empty()
        };
        if !password.is_empty() {
            sess.password = Secret::new(password.to_string());
        }
        out.push(sess);
    }
    out
}

/// (#group-dot 2026-09-14) 同上,但每项**多带该组的颜色**:会话对话框的「分组」
/// 下拉要在组名前画色点,而 Slint 侧既无"按名查色"也无 color→hex 的能力,故在
/// Rust 侧把名称与颜色配成对一起注入(与 named_display_groups 同源)。
/// 未设色的组 hex = "" → Slint 侧只留空位、不画点。
pub(super) fn session_groups_model(store: &ConfigStore) -> ModelRc<GroupEntry> {
    let named = named_display_groups(store.groups(), store.sessions());
    let colors = store.group_colors();
    let entries: Vec<GroupEntry> = named
        .into_iter()
        .map(|name| {
            let hex = colors
                .get(name.as_str())
                .map(|h| h.trim())
                .filter(|h| !h.is_empty())
                .unwrap_or("");
            GroupEntry {
                name: name.as_str().into(),
                hex: hex.into(),
                color: parse_hex_color(hex).unwrap_or_default(),
                // (#group-dot-r2 2026-09-15) 成员数:下拉项右缘展示。
                count: store
                    .sessions()
                    .iter()
                    .filter(|s| s.group == *name)
                    .count() as i32,
            }
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(entries)))
}

/// (#move-to-groups 2026-09-14) 成员右键菜单「移动到」的组清单,顺序 =
/// **默认组恒居首位**,其后是具名组(存储顺序 = 组头拖动维护的顺序)。
///
/// 为什么不再让 Slint 侧遍历 root.sessions 过滤:
///  · `visible: false` 的行**仍然各占一个 VerticalLayout 的 spacing** —— 列表
///    几十行里绝大多数不可见,菜单里就堆出用户看到的"组与组之间留有空隙"
///    (实测:一两行可见却隔着好几个身位);
///  · 默认组在 sessions 里只有当**存在未分组成员**时才有对应行,未分组会话全
///    被移走时菜单里"默认组"整个消失 —— 而它恰恰是最常用的归位目标(用户
///    要求"第一位永远是默认组")。这里直接由存储侧列出全部可用组,与
///    display_groups 同源,不再依赖"恰好有成员"。
/// 排除:builtin 的 system 组(存不了普通会话)。当前会话所在的组不在这里
/// 排除——排除条件依赖"右键的是哪一行",而本模型在会话列表刷新时一次性填好,
/// 由 Slint 侧按 ctx-menu-group 过滤(那里的 `if` 同样不占布局)。
pub(super) fn move_target_groups_model(store: &ConfigStore) -> ModelRc<SharedString> {
    let mut groups: Vec<SharedString> = vec![SharedString::from("default")];
    groups.extend(
        named_display_groups(store.groups(), store.sessions())
            .into_iter()
            .map(SharedString::from),
    );
    ModelRc::from(Rc::new(VecModel::from(groups)))
}

/// Build the jump-host picker's parallel label/id lists for the session dialog
/// (#211). Index 0 is always the "no jump host" entry (empty id); the rest are
/// the saved SSH sessions except `exclude_id` (a session can't jump through
/// itself). Returns `(labels, ids, selected_index)` where `selected_index`
/// points at `current_jump_id` (0 if unset / dangling).
pub(super) fn jump_candidates(
    store: &ConfigStore,
    exclude_id: &str,
    current_jump_id: &str,
) -> (ModelRc<SharedString>, ModelRc<SharedString>, i32) {
    let mut labels: Vec<SharedString> = vec![t("无（直接连接）", "None (direct)").into()];
    let mut ids: Vec<SharedString> = vec!["".into()];
    let mut selected: i32 = 0;
    for s in store.sessions() {
        if s.kind != SessionKind::Ssh || s.id == exclude_id {
            continue;
        }
        let label = if s.name.trim().is_empty() {
            if s.user.trim().is_empty() {
                s.host.clone()
            } else {
                format!("{}@{}", s.user, s.host)
            }
        } else {
            format!("{} ({}@{})", s.name, s.user, s.host)
        };
        if s.id == current_jump_id {
            selected = ids.len() as i32;
        }
        labels.push(label.into());
        ids.push(s.id.clone().into());
    }
    (
        ModelRc::from(Rc::new(VecModel::from(labels))),
        ModelRc::from(Rc::new(VecModel::from(ids))),
        selected,
    )
}

fn normalized_query(query: &str) -> String {
    query.trim().to_lowercase()
}

fn session_matches_normalized_query(session: &Session, query: &str) -> bool {
    query.is_empty()
        || session.name.to_lowercase().contains(query)
        || session.host.to_lowercase().contains(query)
}

#[cfg(test)]
fn session_matches_query(session: &Session, query: &str) -> bool {
    let query = normalized_query(query);
    session_matches_normalized_query(session, &query)
}

fn build_session_rows(
    sessions: &[Session],
    explicit_groups: &[String],
    collapsed_groups: Option<&[String]>,
    // (#group-color 2026-09-14) 用户在右键菜单里设的分组颜色(组名 -> hex)。
    group_colors: &HashMap<String, String>,
    builtin_sessions: &[Session],
    query: &str,
    // (#hide-system-group 2026-09-13) 隐藏"本地终端"保留组:**模型层过滤**。
    // Slint 的 `visible:false` 只隐藏不回收空间,列表上方会残留一整块空白;
    // 在这里把 system 组从数据里剔掉,后面的组自然上移补位。
    hide_system: bool,
) -> Vec<SessionInfo> {
    // Group sessions by their `group` (named groups alphabetically, ungrouped
    // last), then by name within each group, and tag the first row of every
    // group with a header so the welcome list can render a folder heading (#41).
    let query = normalized_query(query);
    // 隐藏时把 system 组从输入里剔掉 —— 组头、成员、显式分组一起消失。
    let owned_sessions: Vec<Session> = if hide_system {
        sessions
            .iter()
            .filter(|s| s.group != "system")
            .cloned()
            .collect()
    } else {
        sessions.to_vec()
    };
    let owned_builtins: Vec<Session> = if hide_system {
        Vec::new()
    } else {
        builtin_sessions.to_vec()
    };
    let owned_groups: Vec<String> = if hide_system {
        explicit_groups
            .iter()
            .filter(|g| g.as_str() != "system")
            .cloned()
            .collect()
    } else {
        explicit_groups.to_vec()
    };
    let sessions = owned_sessions.as_slice();
    let builtin_sessions = owned_builtins.as_slice();
    let explicit_groups = owned_groups.as_slice();
    let searching = !query.is_empty();
    let matches = |session: &Session| session_matches_normalized_query(session, &query);
    let group_is_collapsed = |group: &str| {
        !searching
            && collapsed_groups
                .map(|groups| groups.iter().any(|collapsed| collapsed == group))
                .unwrap_or(true)
    };

    // Ordered list of display groups:
    //  - "default" only when there are ungrouped sessions (group == "")
    //  - named groups: explicit folders (incl. empty ones) ∪ sessions' groups,
    //    de-duplicated, alphabetical.
    let has_default = sessions.iter().any(|session| {
        (session.group.is_empty() || is_reserved_session_group(session.group.trim()))
            && matches(session)
    });
    let mut named: Vec<String> = if searching {
        sessions
            .iter()
            .filter(|session| {
                !session.group.is_empty()
                    && !is_reserved_session_group(session.group.trim())
                    && matches(session)
            })
            .map(|session| session.group.clone())
            .collect()
    } else {
        named_display_groups(explicit_groups, sessions)
    };
    // (#drag-cross-group-fix 2026-09-06) 不再按字母排序:显示顺序必须与
    // named_display_groups 的存储顺序(用户组头拖动换位维护的顺序)完全一致。
    // 旧代码在这里重新按字母排序,导致三处组序互相矛盾:
    //  1) 组头拖动排序(reorder-group 改存储顺序)视觉上无效——显示仍按字母序;
    //  2) 跨组拖动(move-session-delta)按存储顺序找"相邻组",与显示相邻组
    //     不符:存储序为 [测试, Test] 而显示为 [Test, 测试] 时,把"测试"里
    //     的会话向上拖,被判定落到保留组 system,再被保留组检查静默拒绝
    //     ——表现为"首行靠近组别就卡住,永远跨不进上面的组"。
    //  3) 与 reorder_session 的显示序(存储序)不一致,同病。
    // 改为按首次出现去重、保持存储顺序(搜索路径收集的会话组序同样保留)。
    let mut unique: Vec<String> = Vec::new();
    for group in named {
        if !unique.contains(&group) {
            unique.push(group);
        }
    }
    named = unique;

    let mut display_groups: Vec<String> = Vec::new();
    if has_default {
        display_groups.push("default".to_string());
    }
    display_groups.extend(named);

    // (#group-color 2026-09-14) 分组颜色 = **用户在右键菜单里设定的 hex**。
    // 未设 = 无色(has 为 false),由 Slint 回落该主题的次级前景色。
    //
    // 旧的"组名 hash → 8 色调色板"派生机制整体退役:那是临时方案,颜色随机、
    // 且**改个组名就换色**,无法承担"身份标识"的职责。用户定稿:"默认就是
    // 没有色彩的"。落点(组头文件夹 + 成员树线)全部由这一个来源驱动。
    //
    // 返回 (颜色, 是否已设):已设时把 hex 解析成 Color 一并传下去,Slint 侧
    // 不必做字符串→颜色的转换。
    // (#group-color-reserved 2026-09-14) **保留组一视同仁**:system(本地终端)与
    // default(默认组)不再被排除在设色表之外 —— 用户定稿"系统组、默认组都支持
    // 右击修改分组颜色"。二者仍**默认无色**(不参与自动配色),只是用户手动设色
    // 后照常显示。组名为空(内部哨兵)才是真正的无色。
    let group_color = |group: &str| -> (slint::Color, String) {
        if group.is_empty() {
            return (slint::Color::default(), String::new());
        }
        match group_colors.get(group).filter(|hex| !hex.is_empty()) {
            Some(hex) => match parse_hex_color(hex) {
                Some(color) => (color, hex.clone()),
                None => (slint::Color::default(), String::new()),
            },
            None => (slint::Color::default(), String::new()),
        }
    };

    // Placeholder row for an empty folder; id == "" marks it as a group header
    // with no session (used by the UI to gate the "delete group" action).
    // (#group-color 2026-09-14) 空组占位行也要带组色 —— 组头行的文件夹
    // 靠它上色,占位组同样要显示自己的颜色。
    let blank = |group: &str, gc: slint::Color, hex: &str| SessionInfo {
        id: "".into(),
        name: "".into(),
        // Placeholder rows render no member icon; "" falls back to the default
        // protocol glyph. See Theme.protocol-glyph.
        kind: "".into(),
        host: "".into(),
        port: 0,
        user: "".into(),
        group: group.into(),
        group_header: group.into(),
        collapsed: group_is_collapsed(group),
        builtin: false,
        conn_state: 0,
        note: "".into(),
        group_index: 0,
        group_size: 0,
        group_color: gc,
        group_color_hex: hex.into(),
    };

    let mut rows: Vec<SessionInfo> = Vec::new();
    // (#group-count-badge) 先收集再计数:组头行要带"组内成员数"。
    let builtin_matched: Vec<&Session> = builtin_sessions
        .iter()
        .filter(|session| matches(session))
        .collect();
    for (i, s) in builtin_matched.iter().enumerate() {
        // (#group-color-reserved 2026-09-14) 本地终端组默认无色,但支持用户右击
        // 设色 —— 与具名组读同一张表,不再硬编码无色。
        let (sys_gc, sys_hex) = group_color("system");
        rows.push(SessionInfo {
            id: s.id.clone().into(),
            name: s.name.clone().into(),
            kind: s.kind.as_str().into(),
            host: s.host.clone().into(),
            port: 0,
            user: s.user.clone().into(),
            group: "system".into(),
            group_color: sys_gc,
            group_color_hex: sys_hex.as_str().into(),
            group_header: if i == 0 { "system".into() } else { "".into() },
            collapsed: group_is_collapsed("system"),
            note: "".into(),
            builtin: true,
            conn_state: 0,
            group_index: i as i32,
            // (#drag-cross-group-fix 2026-09-06) 组大小写入每一行:拖拽起拖时
            // drag-size 取自"被拖行"的 session.group-size,旧代码只有首行有值
            // (徽章用途),非首行拖拽时 size=0 → Rust 端组内窗口
            // dn∈[-gi, size-1-gi] 塌缩为空,组内排序被误判成跨组移动。
            // 计数徽章只在组头行渲染,非首行带值无副作用。
            group_size: builtin_matched.len() as i32,
        });
    }
    for group in &display_groups {
        let gs: Vec<&Session> = if group == "default" {
            sessions
                .iter()
                .filter(|session| {
                    (session.group.is_empty() || is_reserved_session_group(session.group.trim()))
                        && matches(session)
                })
                .collect()
        } else {
            sessions
                .iter()
                .filter(|session| &session.group == group && matches(session))
                .collect()
        };
        // No alphabetical sort: the stored Vec order is the user's manual
        // order, maintained by drag-to-reorder (same convention as quick
        // commands). New sessions land at the end of their group.
        // (#default-group-header 2026-09-10) default 组现在与具名组同构:有组头、
        // 可折叠、可作跨组拖放的落点。空占位(blank)仍只给具名组——default 组
        // 只在存在未分组成员时才进入 display_groups(见上面的 has_default),
        // 恒有成员,该分支天然走不到。
        // (#group-color 2026-09-14) 本组的颜色,组头与成员行共用同一个值。
        let (gc, hex) = group_color(group);
        if gs.is_empty() && !searching && group != "default" {
            rows.push(blank(group, gc, &hex));
        } else {
            for (i, s) in gs.iter().enumerate() {
                rows.push(SessionInfo {
                    id: s.id.clone().into(),
                    name: s.name.clone().into(),
                    kind: s.kind.as_str().into(),
                    host: s.host.clone().into(),
                    port: s.port as i32,
                    user: s.user.clone().into(),
                    note: s.note.clone().into(),
                    group: group.clone().into(),
                    group_color: gc,
                    group_color_hex: hex.as_str().into(),
                    // (#default-group-header 2026-09-10) default 组现在也生成
                    // 组头行:未分组的会话不再是"顶层平铺的单独会话",而是挂在
                    // "默认组"下面的普通成员(用户要求:每个会话必须属于某个组,
                    // 没写组的归入默认组)。原 `group != "default"` 判断是
                    // "未分组平铺"那一版的遗留。
                    group_header: if i == 0 {
                        group.clone().into()
                    } else {
                        "".into()
                    },
                    // (#default-group-header) 折叠不再是特例:默认组与其他组
                    // 一样可展开/折叠,折叠状态照常读写——它现在有组头可以点
                    // 开,不会再出现"折叠记录把成员永久藏住"的死局。
                    collapsed: group_is_collapsed(group),
                    builtin: false,
                    conn_state: 0,
                    group_index: i as i32,
                    // (#drag-cross-group-fix 2026-09-06) 同上:非首行也带
                    // 真实组大小,否则非首行拖拽的组内/跨组判定全部失真。
                    group_size: gs.len() as i32,
                });
            }
        }
    }
    rows
}

pub(super) fn sync_sessions_to_model_with_filter(
    store: &ConfigStore,
    model: &VecModel<SessionInfo>,
    query: &str,
) {
    let builtin_sessions = builtin_local_sessions(store.wsl_profiles());
    // (#hide-system-group) 隐藏"本地终端"时把 system 组整组从**模型**里剔除:
    // Slint 的 `visible: false` 只是不画、位置照占,列表上方会残留一块空白。
    let mut rows = build_session_rows(
        store.sessions(),
        store.groups(),
        store.collapsed_session_groups(),
        store.group_colors(),
        &builtin_sessions,
        query,
        store.system_group_hidden(),
    );
    model.set_vec(rows);
}

/// Same rows as `sync_sessions_to_model_with_filter`, but when the row count
/// is unchanged the rows are written with `set_row_data` instead of `set_vec`:
/// the `for` loop keeps its elements (and a drag's pointer grab) alive. Used
/// for per-hop updates during drag-to-reorder. Returns false when the row
/// count changed and a full `set_vec` rebuild was required — that recreates
/// the rows and drops the dragging row's pointer grab.
pub(super) fn refresh_session_rows_in_place(
    store: &ConfigStore,
    model: &VecModel<SessionInfo>,
    query: &str,
) -> bool {
    use slint::Model as _;
    let builtin_sessions = builtin_local_sessions(store.wsl_profiles());
    let mut rows = build_session_rows(
        store.sessions(),
        store.groups(),
        store.collapsed_session_groups(),
        store.group_colors(),
        &builtin_sessions,
        query,
        store.system_group_hidden(),
    );
    if rows.len() == model.row_count() {
        for (i, row) in rows.into_iter().enumerate() {
            model.set_row_data(i, row);
        }
        true
    } else {
        model.set_vec(rows);
        false
    }
}

pub(super) fn sync_sessions_to_model(store: &ConfigStore, model: &VecModel<SessionInfo>) {
    sync_sessions_to_model_with_filter(store, model, "");
}

pub(super) fn builtin_local_sessions(wsl_profiles: &[crate::config::WslProfile]) -> Vec<Session> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        out.push(builtin_local_session(
            "system:powershell",
            "PowerShell",
            "powershell",
        ));
        out.push(builtin_local_session("system:cmd", "CMD", "cmd"));
        if wsl_available() {
            if wsl_profiles.is_empty() {
                let mut session = builtin_local_session("system:wsl", "WSL", "wsl");
                session.local_working_dir = "~".to_string();
                out.push(session);
            } else {
                for profile in wsl_profiles {
                    let mut session = builtin_local_session(
                        &format!("system:wsl:{}", profile.id),
                        profile.name.clone(),
                        "wsl",
                    );
                    session.local_distribution = profile.distribution.clone();
                    session.local_working_dir = if profile.directory.trim().is_empty() {
                        "~".to_string()
                    } else {
                        profile.directory.clone()
                    };
                    out.push(session);
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Shell")
            .to_string();
        out.push(builtin_local_session("system:shell", name, "shell"));
    }
    out
}

pub(super) fn builtin_local_session(id: &str, name: impl Into<String>, host: &str) -> Session {
    let mut s = Session::new_empty();
    s.id = id.to_string();
    s.name = name.into();
    s.host = host.to_string();
    s.user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    s.group = "system".to_string();
    s.kind = SessionKind::Local;
    s
}

#[cfg(windows)]
pub(super) fn wsl_available() -> bool {
    use std::os::windows::process::CommandExt;

    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("wsl.exe")
            .arg("--status")
            .creation_flags(0x08000000)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Session callbacks (welcome page + dialog)
// ---------------------------------------------------------------------------

/// Build the effective session represented by the dialog. When editing, blank
/// secret fields retain their saved values because real passwords and pasted
/// private keys are deliberately never echoed back into the UI (#10, #276).
pub(super) fn session_from_draft(
    draft: &SessionDraft,
    existing: Option<&Session>,
    forwards: Vec<crate::config::PortForward>,
    triggers: Vec<crate::config::SessionTrigger>,
) -> Session {
    let password = if draft.password.is_empty() {
        existing.map(|s| s.password.clone()).unwrap_or_default()
    } else {
        Secret::new(draft.password.to_string())
    };
    let private_key_inline = if draft.private_key_inline_mode {
        if draft.private_key_inline.is_empty() {
            existing
                .map(|s| s.private_key_inline.clone())
                .unwrap_or_default()
        } else {
            Secret::new(draft.private_key_inline.to_string())
        }
    } else {
        Secret::default()
    };
    let private_key_path = if draft.private_key_inline_mode {
        String::new()
    } else {
        draft.private_key_path.to_string().replace('\\', "/")
    };
    let kind = SessionKind::from_str(&draft.kind.to_string());
    let auto_name = match kind {
        SessionKind::Serial => format!("{} @{}", draft.serial_port, draft.baud_rate),
        _ if draft.user.trim().is_empty() => draft.host.to_string(),
        _ => format!("{}@{}", draft.user, draft.host),
    };
    let default_port = if kind == SessionKind::Telnet { 23 } else { 22 };

    Session {
        id: draft.id.to_string(),
        name: if draft.name.is_empty() {
            auto_name
        } else {
            draft.name.to_string()
        },
        host: draft.host.to_string(),
        port: if draft.port <= 0 {
            default_port
        } else {
            draft.port as u16
        },
        user: draft.user.to_string(),
        auth: AuthMethod::from_str(&draft.auth.to_string()),
        password,
        private_key_path,
        private_key_inline,
        proxy: draft.proxy.to_string(),
        last_used: None,
        group: draft.group.to_string(),
        kind,
        local_distribution: String::new(),
        local_working_dir: String::new(),
        serial_port: draft.serial_port.to_string(),
        baud_rate: if draft.baud_rate <= 0 {
            115_200
        } else {
            draft.baud_rate as u32
        },
        data_bits: draft.data_bits as u8,
        stop_bits: draft.stop_bits as u8,
        parity: draft.parity.to_string(),
        flow_control: draft.flow_control.to_string(),
        encoding: draft.encoding.to_string(),
        vt100_drawing: draft.vt100_drawing,
        forwards,
        triggers,
        disable_shell_integration: draft.disable_shell_integration,
        note: draft.note.to_string(),
        jump_session_id: draft.jump_session_id.to_string(),
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    fn session(id: &str, name: &str, host: &str, group: &str) -> Session {
        let mut value = Session::new_empty();
        value.id = id.into();
        value.name = name.into();
        value.host = host.into();
        value.group = group.into();
        value
    }

    #[test]
    fn session_search_matches_name_and_host_case_insensitively() {
        let value = session("1", "Prod API", "DB.EXAMPLE.COM", "prod");
        assert!(session_matches_query(&value, "  prod  "));
        assert!(session_matches_query(&value, "example.com"));
        assert!(!session_matches_query(&value, "staging"));
    }

    #[test]
    fn filtered_rows_hide_empty_groups_and_expand_matches() {
        let saved = vec![session("1", "Prod API", "10.0.0.8", "prod")];
        let builtins = vec![session("local", "Local terminal", "localhost", "system")];
        let groups = vec!["empty".to_string(), "prod".to_string()];
        let collapsed = vec!["prod".to_string(), "system".to_string()];

        let rows = build_session_rows(&saved, &groups, Some(&collapsed), &HashMap::new(), &builtins, "prod", false);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name.as_str(), "Prod API");
        assert_eq!(rows[0].group_header.as_str(), "prod");
        assert!(!rows[0].collapsed);
    }

    #[test]
    fn filtered_rows_include_matching_builtin_sessions() {
        let builtins = vec![session("local", "Local terminal", "localhost", "system")];

        let rows = build_session_rows(
            &[],
            &[],
            Some(&["system".to_string()]),
            &HashMap::new(),
            &builtins,
            "LOCALHOST",
            false,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name.as_str(), "Local terminal");
        assert_eq!(rows[0].group_header.as_str(), "system");
        assert!(rows[0].builtin);
        assert!(!rows[0].collapsed);
    }

    #[test]
    fn filtered_rows_are_empty_when_nothing_matches() {
        let saved = vec![session("1", "Prod API", "10.0.0.8", "prod")];
        let builtins = vec![session("local", "Local terminal", "localhost", "system")];

        let rows = build_session_rows(&saved, &[], None, &HashMap::new(), &builtins, "staging", false);

        assert!(rows.is_empty());
    }

    #[test]
    fn empty_query_restores_saved_groups_and_collapse_state() {
        let saved = vec![session("1", "Prod API", "10.0.0.8", "prod")];
        let groups = vec!["empty".to_string(), "prod".to_string()];
        let collapsed = vec!["prod".to_string()];

        let rows = build_session_rows(&saved, &groups, Some(&collapsed), &HashMap::new(), &[], "", false);

        assert!(rows
            .iter()
            .any(|row| row.group.as_str() == "empty" && row.id.is_empty()));
        assert!(rows
            .iter()
            .any(|row| row.group.as_str() == "prod" && row.collapsed));
    }
}

#[cfg(test)]

mod drag_order_tests {
    use super::*;

    fn sess(id: &str, name: &str, group: &str) -> Session {
        let mut value = Session::new_empty();
        value.id = id.into();
        value.name = name.into();
        value.group = group.into();
        value
    }

    /// (#drag-cross-group-fix 2026-09-06) 显示组序必须等于存储组序(组头拖动
    /// 换位维护的顺序),不得按字母重排——否则跨组拖动的"相邻组"判定与显示
    /// 相邻组不符,向上拖会被判进保留组 system 而静默 no-op(首行卡在组边
    /// 界)。且每一行都要携带真实 group_size:非首行旧实现填 0,拖拽起拖时
    /// drag-size 取自被拖行,组内窗口 dn∈[-gi, size-1-gi] 塌缩,组内排序
    /// 被误判成跨组移动。
    #[test]
    fn group_rows_follow_stored_order_and_carry_group_size() {
        let saved = vec![
            sess("1", "202", "测试"),
            sess("2", "s", "Test"),
            sess("3", "t2", "Test"),
        ];
        // 存储顺序:测试 在 Test 之前(字母序会把它排到后面)。
        let groups = vec!["测试".to_string(), "Test".to_string()];

        let rows = build_session_rows(&saved, &groups, None, &HashMap::new(), &[], "", false);

        let headers: Vec<&str> = rows
            .iter()
            .filter(|r| !r.group_header.is_empty())
            .map(|r| r.group_header.as_str())
            .collect();
        assert_eq!(headers, ["测试", "Test"]);

        for row in rows.iter().filter(|r| r.group.as_str() == "Test") {
            assert_eq!(row.group_size, 2, "every row must carry the real group size");
        }
        let first_test = rows.iter().find(|r| r.group.as_str() == "Test").unwrap();
        assert_eq!(first_test.group_header.as_str(), "Test");
        assert_eq!(first_test.group_index, 0);
        let second_test = rows
            .iter()
            .filter(|r| r.group.as_str() == "Test")
            .nth(1)
            .unwrap();
        assert_eq!(second_test.group_index, 1);
    }
}

#[cfg(test)]
mod row_display_tests {
    use super::*;

    // (#proto-icon 2026-09-17) 每一行的 kind 必须原样带出协议名 —— 它是成员行
    // 图标(Theme.protocol-glyph)的唯一数据源。各协议各测一遍。
    #[test]
    fn rows_carry_the_protocol_kind_for_the_row_icon() {
        for (kind, expected) in [
            (SessionKind::Ssh, "ssh"),
            (SessionKind::Serial, "serial"),
            (SessionKind::Telnet, "telnet"),
            (SessionKind::Local, "local"),
        ] {
            let mut session = Session::new_empty();
            session.kind = kind;
            let rows = build_session_rows(&[session], &[], None, &HashMap::new(), &[], "", false);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].kind.as_str(), expected);
        }
    }

    // 内置本地会话(builtin)走另一条构造分支,kind 同样要带出来(local)。
    #[test]
    fn builtin_rows_carry_local_kind() {
        let builtin = builtin_local_session("id", "PowerShell", "powershell");
        let rows = build_session_rows(&[], &[], None, &HashMap::new(), &[builtin], "", false);
        let row = rows
            .iter()
            .find(|r| r.builtin && r.name.as_str() == "PowerShell")
            .expect("builtin row");
        assert_eq!(row.kind.as_str(), "local");
    }

    #[test]
    fn network_rows_keep_their_address_fields() {
        for kind in [SessionKind::Ssh, SessionKind::Telnet] {
            let mut session = Session::new_empty();
            session.kind = kind;
            session.host = "example.com".into();
            session.port = 2222;
            session.user = "alice".into();
            let rows = build_session_rows(&[session], &[], None, &HashMap::new(), &[], "", false);
            assert_eq!(rows[0].host.as_str(), "example.com");
            assert_eq!(rows[0].port, 2222);
            assert_eq!(rows[0].user.as_str(), "alice");
        }
    }
}
