// 始终使用 Windows GUI 子系统：避免 debug/提权启动时出现额外的黑色控制台窗口。
// 需要看 Rust 日志时，可临时改为 `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`。
#![windows_subsystem = "windows"]

fn main() {
    windows_clean_lib::run()
}
