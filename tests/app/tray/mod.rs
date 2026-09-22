//! (#tray-click-pos 2026-09-23) `GetMessagePos` 拆包纯函数单测:低 16 位 = x、
//! 高 16 位 = y,且都必须按 i16 符号扩展 —— 虚拟桌面坐标系下副屏在主屏
//! 左侧/上方时坐标为负。
#![cfg(windows)]

use super::split_message_pos;

#[test]
fn splits_positive_screen_coords() {
    let pos: u32 = 500 | (300 << 16);
    assert_eq!(split_message_pos(pos), (500, 300));
}

#[test]
fn sign_extends_negative_virtual_desktop_coords() {
    // 副屏在主屏左/上方:坐标为负。若不符号扩展,-320 会被读成 65216。
    let pos: u32 = ((-320i32) as u16 as u32) | (((-192i32) as u16 as u32) << 16);
    assert_eq!(split_message_pos(pos), (-320, -192));
}

#[test]
fn splits_i16_boundary_coords() {
    // i16 正负边界:32767 保持,-32768 须正确符号扩展。
    let pos: u32 = 32767u32 | (u32::from((-32768i16) as u16) << 16);
    assert_eq!(split_message_pos(pos), (32767, -32768));
}
