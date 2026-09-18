use super::super::*;

#[test]
fn close_button_asks_even_without_live_sessions() {
    assert_eq!(decide_close("ask"), CloseDecision::Ask);
    assert_eq!(decide_close("tray"), CloseDecision::Tray);
    assert_eq!(decide_close("exit"), CloseDecision::Exit);
}
