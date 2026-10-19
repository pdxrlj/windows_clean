// 发布版（release）不显示控制台窗口；debug 保留控制台便于查看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    windows_clean_lib::run()
}
