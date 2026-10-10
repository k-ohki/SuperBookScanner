//! Tauri のバックエンド (Mac 版・Windows 版で共通)。画像処理はすべて book-core に任せ、ここでは画面とのやりとりだけを行う。
//! 各 OS のアプリ (mac/src-tauri, windows/src-tauri) は、自分の tauri.conf.json から作った Context を渡して `run` を呼ぶ。
//!
//! 処理の本体は `ops` にあり、同じ処理を `remote` (同じ Wi-Fi の iPad などのブラウザから使う Web サーバー) からも呼ぶ。

mod ops;
mod remote;

use book_core::pipeline::Progress;
use book_core::project::PageOverride;
use ops::{FolderInfo, Preview, Settings};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

/// アプリ全体で 1 つの状態。書き出しの中止フラグは、アプリの画面とブラウザで共有する。
#[derive(Default)]
pub(crate) struct AppState {
    pub cancel: Arc<AtomicBool>,
    pub remote: remote::RemoteState,
}

/// 重い処理を別スレッドで実行し、エラーは画面に出せる文字列にする。
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn open_folder(path: String) -> Result<FolderInfo, String> {
    ops::open_folder(&path).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn save_override(folder: String, file: String, value: PageOverride) -> Result<(), String> {
    ops::save_override(&folder, &file, value).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn thumbnail(path: String, max_side: u32, rotate: u16) -> Result<String, String> {
    blocking(move || ops::thumbnail(&path, max_side, rotate)).await
}

#[tauri::command]
async fn preview(path: String, settings: Settings, value: PageOverride, max_side: u32) -> Result<Preview, String> {
    blocking(move || ops::preview(&path, &settings, &value, max_side)).await
}

/// PDF に書き出す。進捗は "convert-progress" イベントで画面に送る。
#[tauri::command]
async fn convert(app: AppHandle, state: State<'_, AppState>, input: String, output: String, settings: Settings) -> Result<(), String> {
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    blocking(move || {
        let progress = |p: Progress| {
            let _ = app.emit("convert-progress", p);
        };
        ops::convert(&input, &output, &settings, &progress, &cancel)
    })
    .await
}

#[tauri::command]
fn cancel_convert(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

/// 起動時の引数でフォルダが渡されていれば、そのパス (`superbook-mac <フォルダ>` で起動した場合)。
#[tauri::command]
fn initial_folder() -> Option<String> {
    std::env::args().skip(1).find(|a| Path::new(a).is_dir())
}

#[tauri::command]
fn sharpen_available() -> Option<String> {
    ops::sharpen_available()
}

/// フォルダがなければ作る (PDF の保存先)。
#[tauri::command]
fn ensure_dir(path: String) -> Result<(), String> {
    std::fs::create_dir_all(&path).map_err(|e| format!("{path}: {e}"))
}

/// iPad などから使えるようにする (同じネットワーク内に Web サーバーを立てる)。
#[tauri::command]
async fn remote_start(app: AppHandle, state: State<'_, AppState>) -> Result<remote::RemoteInfo, String> {
    remote::start(app, &state).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn remote_stop(state: State<'_, AppState>) {
    remote::stop(&state);
}

/// 動いていれば接続先などの情報。
#[tauri::command]
fn remote_status(state: State<'_, AppState>) -> Option<remote::RemoteInfo> {
    remote::status(&state)
}

pub fn run(context: tauri::Context) {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            // 環境変数 SUPERBOOK_REMOTE=1 で起動すると、最初から iPad などから使える状態にする
            if std::env::var_os("SUPERBOOK_REMOTE").is_some_and(|v| v == "1") {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<AppState>();
                    if let Err(e) = remote::start(handle.clone(), &state).await {
                        eprintln!("remote: {e:#}");
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_folder,
            save_override,
            thumbnail,
            preview,
            convert,
            cancel_convert,
            sharpen_available,
            ensure_dir,
            initial_folder,
            remote_start,
            remote_stop,
            remote_status
        ])
        .run(context)
        .expect("error while running tauri application");
}
