use super::*;
// (#drag-cross-group-fix 2026-09-06) 显式导入:此前经 `use super::*` 隐式
// 继承 app.rs 的 use 项,app.rs 内不再直接使用该函数后需在此声明。
use crate::config::named_display_groups;

fn serial_session_detail(session: &Session) -> String {
    if session.kind != SessionKind::Serial {
        return String::new();
    }
    let parity = match session.parity.as_str() {
        "odd" => "O",
        "even" => "E",
        _ => "N",
    };
    format!(
        "{} · {} baud · {}{}{}",
        session.serial_port, session.baud_rate, session.data_bits, parity, session.stop_bits
    )
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

/// Distinct named groups (explicit folders ∪ the groups sessions are filed under),
/// de-duplicated and sorted alphabetically — feeds the new/edit dialog's group
/// dropdown (#179). Ungrouped ("") is excluded; the dialog leaves the field blank
/// for that case.
pub(super) fn session_groups_model(store: &ConfigStore) -> ModelRc<SharedString> {
    let named = named_display_groups(store.groups(), store.sessions());
    ModelRc::from(Rc::new(VecModel::from(
        named
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )))
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
    builtin_sessions: &[Session],
    query: &str,
) -> Vec<SessionInfo> {
    // Group sessions by their `group` (named groups alphabetically, ungrouped
    // last), then by name within each group, and tag the first row of every
    // group with a header so the welcome list can render a folder heading (#41).
    let query = normalized_query(query);
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

    // (#first-group-no-pending 2026-09-10) 列表最顶的【可进入组】:其上方没有
    // 任何可进入的组,拖拽上移时应跳过"待出组(pending)"直接进"出组"态。
    //
    // (#default-group-drop 2026-09-10) 判定基准由"第一个具名组"改为
    // display_groups.first()。默认组成为正式组后(有组头、可折叠、可作落点),
    // 当它存在时它就是最顶可进入组——其上方只有不可进入的 builtin(本地终端)。
    // 旧实现 find(|g| g != "default") 跳过 default 取第一个具名组,在两个场景
    // 同时错位:① default 组上移仍走 pending(上方无组可进却提示"松手回原位");
    // ② 具名组上移被误判为最顶组而跳过 pending,但它上方明明有 default 可进入
    // ——组名不亮蓝框、也拿不到"回原位"提示,落点反馈缺失。
    let first_group: Option<String> = display_groups.first().cloned();

    // (#group-hue 2026-09-10) 组色相索引:按【组名】做 FNV-1a 32 位 hash 取模。
    // 为什么放 Rust:Slint 1.8 的字符串 API 只有 length / is-empty,拿不到字符,
    // 纯 .slint 侧只能按"组名长度"取色,而 "3"/"1"/"2" 这类单字组名会全部
    // 撞成同一色,不可用。
    // 为什么按组名而不是序号:hash 与排序位置无关,组被拖动到任何位置都保持
    // 原色,否则用户一排序颜色就变,颜色失去"身份标识"的意义。
    // 返回值即 Theme.group-hue-* 调色板下标;-1 = 无组(group == ""),Slint 侧
    // 回落中性描边色。调色板长度变更时只需同步此处的 HUE_COUNT。
    const HUE_COUNT: u32 = 8;
    let group_hue = |group: &str| -> i32 {
        // 无组 / 默认组(「默认组」)一律回落中性描边色(Slint 侧 -1 →
        // #3a3d46)。(#default-group-hue 2026-09-10) 默认组不是"身份分组",
        // 而是未填写分组会话的收容处:给它 hash 随机色会让它看起来像一个
        // 具名组,用户会误以为它跟 3/1/2 那些组是同一类东西(用户定稿:
        // 默认组头保持中性色)。
        if group.is_empty() || group.eq_ignore_ascii_case("default") {
            return -1;
        }
        let mut h: u32 = 0x811c9dc5; // FNV-1a offset basis
        for b in group.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(0x01000193); // FNV prime
        }
        // 雪崩混合(murmur3 finalizer)。必须做:FNV 的**低位**分布很差,
        // 直接 `% 8` 只取低 3 位,实测 16 个常见组名只落到 7 档且严重偏斜
        // (system / 1 / prod / dev 全挤在同一档)。混合后 8 档全部用上,
        // 且 system(本地终端)稳定独占 0 号绿。
        h ^= h >> 16;
        h = h.wrapping_mul(0x85ebca6b);
        h ^= h >> 13;
        h = h.wrapping_mul(0xc2b2ae35);
        h ^= h >> 16;
        (h % HUE_COUNT) as i32
    };

    // Placeholder row for an empty folder; id == "" marks it as a group header
    // with no session (used by the UI to gate the "delete group" action).
    let blank = |group: &str| SessionInfo {
        id: "".into(),
        name: "".into(),
        host: "".into(),
        serial_detail: "".into(),
        port: 0,
        user: "".into(),
        auth: "".into(),
        last_used: "".into(),
        group: group.into(),
        group_header: group.into(),
        collapsed: group_is_collapsed(group),
        builtin: false,
        conn_state: 0,
        note: "".into(),
        group_index: 0,
        group_size: 0,
        // (#first-group-no-pending) 空组占位行同样按组归属标记。
        first_group: first_group.as_deref() == Some(group),
        group_hue: group_hue(group),
    };

    let mut rows: Vec<SessionInfo> = Vec::new();
    // (#group-count-badge) 先收集再计数:组头行要带"组内成员数"。
    let builtin_matched: Vec<&Session> = builtin_sessions
        .iter()
        .filter(|session| matches(session))
        .collect();
    for (i, s) in builtin_matched.iter().enumerate() {
        rows.push(SessionInfo {
            id: s.id.clone().into(),
            name: s.name.clone().into(),
            host: s.host.clone().into(),
            serial_detail: "".into(),
            port: 0,
            user: s.user.clone().into(),
            auth: s.kind.as_str().into(),
            last_used: "".into(),
            group: "system".into(),
            group_hue: group_hue("system"),
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
            // (#first-group-no-pending) builtin(本地终端)不参与。
            first_group: false,
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
        if gs.is_empty() && !searching && group != "default" {
            rows.push(blank(group));
        } else {
            for (i, s) in gs.iter().enumerate() {
                rows.push(SessionInfo {
                    id: s.id.clone().into(),
                    name: s.name.clone().into(),
                    host: s.host.clone().into(),
                    serial_detail: serial_session_detail(s).into(),
                    port: s.port as i32,
                    user: s.user.clone().into(),
                    auth: s.auth.as_str().into(),
                    note: s.note.clone().into(),
                    last_used: s
                        .last_used
                        .clone()
                        .unwrap_or_else(|| "never".to_string())
                        .into(),
                    group: group.clone().into(),
                    group_hue: group_hue(group),
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
                    // (#first-group-no-pending 2026-09-10) 该组是否为列表最顶
                    // 的【可进入组】(拖拽跳过 pending 的依据;默认组存在时即
                    // 默认组)。
                    first_group: first_group.as_deref() == Some(group.as_str()),
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
    model.set_vec(build_session_rows(
        store.sessions(),
        store.groups(),
        store.collapsed_session_groups(),
        &builtin_sessions,
        query,
    ));
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
    let rows = build_session_rows(
        store.sessions(),
        store.groups(),
        store.collapsed_session_groups(),
        &builtin_sessions,
        query,
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

        let rows = build_session_rows(&saved, &groups, Some(&collapsed), &builtins, "prod");

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
            &builtins,
            "LOCALHOST",
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

        let rows = build_session_rows(&saved, &[], None, &builtins, "staging");

        assert!(rows.is_empty());
    }

    #[test]
    fn empty_query_restores_saved_groups_and_collapse_state() {
        let saved = vec![session("1", "Prod API", "10.0.0.8", "prod")];
        let groups = vec!["empty".to_string(), "prod".to_string()];
        let collapsed = vec!["prod".to_string()];

        let rows = build_session_rows(&saved, &groups, Some(&collapsed), &[], "");

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

        let rows = build_session_rows(&saved, &groups, None, &[], "");

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

mod serial_display_tests {
    use super::*;

    #[test]
    fn serial_rows_show_device_and_framing_instead_of_ssh_defaults() {
        for (device, baud, bits, parity, stops, expected) in [
            ("/dev/ttyUSB0", 115200, 8, "none", 1, "/dev/ttyUSB0 · 115200 baud · 8N1"),
            ("COM3", 9600, 7, "even", 2, "COM3 · 9600 baud · 7E2"),
            ("/dev/ttyS0", 57600, 8, "odd", 1, "/dev/ttyS0 · 57600 baud · 8O1"),
        ] {
            let mut session = Session::new_empty();
            session.kind = SessionKind::Serial;
            session.serial_port = device.into();
            session.baud_rate = baud;
            session.data_bits = bits;
            session.parity = parity.into();
            session.stop_bits = stops;
            let rows = build_session_rows(&[session], &[], None, &[], "");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].serial_detail.as_str(), expected);
        }
    }

    #[test]
    fn network_rows_keep_their_address_fields() {
        for kind in [SessionKind::Ssh, SessionKind::Telnet] {
            let mut session = Session::new_empty();
            session.kind = kind;
            session.host = "example.com".into();
            session.port = 2222;
            session.user = "alice".into();
            let rows = build_session_rows(&[session], &[], None, &[], "");
            assert!(rows[0].serial_detail.is_empty());
            assert_eq!(rows[0].host.as_str(), "example.com");
            assert_eq!(rows[0].port, 2222);
            assert_eq!(rows[0].user.as_str(), "alice");
        }

    }

}
