// リリースビルドで Windows のコンソールを出さない (macOS では影響なし)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    superbook_app_lib::run()
}
