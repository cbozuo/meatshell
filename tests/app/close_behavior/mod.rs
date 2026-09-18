//! (#close-behavior) 关闭按钮行为的纯函数单测。
//!
//! 只覆盖不碰 UI/系统的部分:确认卡回传档位的解析、关闭行为配置的
//! 归一化、以及 (mode → 弹卡/托盘/退出) 决策表。窗口回调本身仍依赖
//! 真实窗口,不在单测范围。

use crate::app::{
    close_behavior_or_default, close_mode_remembers, decide_close, parse_close_mode, CloseDecision,
};

#[test]
fn close_mode_parses_tray_and_exit() {
    assert_eq!(parse_close_mode("tray"), "tray");
    assert_eq!(parse_close_mode("exit"), "exit");
}

#[test]
fn close_mode_tolerates_remember_suffix() {
    // 用户勾了「记住我的选择」再点卡片时,Slint 回传带后缀的参数。
    assert_eq!(parse_close_mode("tray,remember"), "tray");
    assert_eq!(parse_close_mode("exit,remember"), "exit");
    assert!(close_mode_remembers("tray,remember"));
    assert!(close_mode_remembers("exit,remember"));
    assert!(!close_mode_remembers("tray"));
    assert!(!close_mode_remembers("exit"));
}

#[test]
fn close_mode_falls_back_to_exit_on_unknown() {
    // 契约只有 tray / exit。空值与无法识别的值都按「完全退出」处理 ——
    // 宁可退出也不要把「想关掉」误读成「留在后台」。
    assert_eq!(parse_close_mode(""), "exit");
    assert_eq!(parse_close_mode("something-else"), "exit");
    assert_eq!(parse_close_mode("TRAY"), "exit"); // 大小写敏感:只有小写是契约值
    // 参数形态是「档位 [+ ,remember 后缀]」,故前缀匹配:仍归到对应档位。
    assert_eq!(parse_close_mode("tray,remember"), "tray");
}

#[test]
fn close_behavior_normalizes_legacy_and_invalid_values() {
    // 旧配置没有该字段 → 空串 → 回落 "ask"(等价于改动前的行为)。
    assert_eq!(close_behavior_or_default(""), "ask");
    assert_eq!(close_behavior_or_default("ask"), "ask");
    assert_eq!(close_behavior_or_default("tray"), "tray");
    assert_eq!(close_behavior_or_default("exit"), "exit");
    // 脏值不该被当成有效档位(否则会静默改变关窗行为)。
    assert_eq!(close_behavior_or_default("TRAY"), "ask");
    assert_eq!(close_behavior_or_default("quit"), "ask");
    assert_eq!(close_behavior_or_default("ask,remember"), "ask");
}

#[test]
fn decide_close_ignores_session_count() {
    // 「每次询问」在 0 个活动会话时也要弹卡,不能 hide() 把窗口藏掉。
    assert_eq!(decide_close("ask"), CloseDecision::Ask);
    assert_eq!(decide_close(""), CloseDecision::Ask);
    assert_eq!(decide_close("tray"), CloseDecision::Tray);
    assert_eq!(decide_close("exit"), CloseDecision::Exit);
    assert_eq!(decide_close("quit"), CloseDecision::Ask);
}
