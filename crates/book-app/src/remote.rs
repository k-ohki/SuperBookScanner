//! 同じネットワーク (家庭内の Wi-Fi) の iPad などのブラウザから、このアプリを使うための Web サーバー。
//!
//! - 画面はアプリと同じもの (ui/) をそのまま配る。画面は `/api/<コマンド名>` に JSON を POST して処理を呼ぶ
//! - 写真はブラウザからアップロードし、「ライブラリ」(書類フォルダの SuperBookScanner/) に 1 冊 1 フォルダで保存する
//! - 書き出した PDF はライブラリに `<本の名前>.pdf` で保存し、ブラウザで開ける
//! - 合言葉などの認証はない (家庭内で使う前提)。そのかわり、読み書きできるのはライブラリの中だけにする

use crate::ops::{self, Settings};
use crate::AppState;
use anyhow::{bail, Context, Result};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as UrlPath, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use book_core::pipeline::Progress;
use book_core::project::PageOverride;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tokio::sync::{broadcast, oneshot};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// 待ち受けるポート。iPad から開く URL に入るので固定にする
pub const PORT: u16 = 8765;

#[derive(Default)]
pub struct RemoteState {
    running: Mutex<Option<Running>>,
}

struct Running {
    info: RemoteInfo,
    shutdown: oneshot::Sender<()>,
    _awake: KeepAwake,
}

/// アプリの画面に出す接続先の情報。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInfo {
    /// iPad の Safari で開く URL (Mac の IP アドレスごと)
    pub urls: Vec<String>,
    /// 最初の URL の QR コード (SVG)
    pub qr_svg: String,
    /// 写真と PDF を保存するフォルダ
    pub library: String,
}

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    library: PathBuf,
    cancel: Arc<AtomicBool>,
    progress: broadcast::Sender<Progress>,
}

pub async fn start(app: AppHandle, state: &AppState) -> Result<RemoteInfo> {
    if let Some(info) = status(state) {
        return Ok(info);
    }
    let library = app.path().document_dir().context("書類フォルダが見つかりません")?.join("SuperBookScanner");
    std::fs::create_dir_all(&library).with_context(|| format!("{} を作れません", library.display()))?;
    let library = library.canonicalize()?;

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", PORT))
        .await
        .with_context(|| format!("ポート {PORT} を使えません (ほかのアプリが使っている可能性があります)"))?;

    let ctx = Ctx {
        app,
        library: library.clone(),
        cancel: state.cancel.clone(),
        progress: broadcast::channel(256).0,
    };
    let router = Router::new()
        .route("/api/events", get(events))
        .route("/api/books", post(books))
        .route("/api/create_book", post(create_book))
        .route("/api/upload/{book}", post(upload))
        .route("/api/download/{book}", get(download))
        .route("/api/open_folder", post(open_folder))
        .route("/api/save_override", post(save_override))
        .route("/api/thumbnail", post(thumbnail))
        .route("/api/preview", post(preview))
        .route("/api/convert", post(convert))
        .route("/api/cancel_convert", post(cancel_convert))
        .route("/api/sharpen_available", post(sharpen_available))
        .route("/api/initial_folder", post(|| async { Json(None::<String>) }))
        .fallback(get(asset))
        // iPhone / iPad の写真は 1 枚数 MB あり、まとめて送るので上限を広げる
        .layer(DefaultBodyLimit::max(1 << 30))
        .with_state(ctx);

    let (tx, rx) = oneshot::channel::<()>();
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });

    let urls: Vec<String> = local_ipv4().iter().map(|ip| format!("http://{ip}:{PORT}/")).collect();
    let qr_svg = urls.first().map(|u| qr_svg(u)).transpose()?.unwrap_or_default();
    let info = RemoteInfo {
        urls,
        qr_svg,
        library: library.display().to_string(),
    };
    *state.remote.running.lock().unwrap() = Some(Running {
        info: info.clone(),
        shutdown: tx,
        _awake: KeepAwake::new(),
    });
    Ok(info)
}

pub fn stop(state: &AppState) {
    if let Some(r) = state.remote.running.lock().unwrap().take() {
        let _ = r.shutdown.send(());
    }
}

pub fn status(state: &AppState) -> Option<RemoteInfo> {
    state.remote.running.lock().unwrap().as_ref().map(|r| r.info.clone())
}

/// サーバーを動かしている間、macOS がアプリを休ませたり (App Nap)、スリープしたりしないようにする。
/// iPad から使うときは、Mac のアプリは裏に回っていることが多く、休ませられると処理が何倍も遅くなるため。
#[cfg(target_os = "macos")]
struct KeepAwake(objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2::runtime::NSObjectProtocol>>);

// 作った後は持っているだけで、終わったら endActivity を呼ぶだけなので、別スレッドに渡しても問題ない
#[cfg(target_os = "macos")]
unsafe impl Send for KeepAwake {}

#[cfg(target_os = "macos")]
impl KeepAwake {
    fn new() -> Self {
        use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
        let reason = NSString::from_str("iPad などからの操作を受け付けています");
        KeepAwake(NSProcessInfo::processInfo().beginActivityWithOptions_reason(NSActivityOptions::UserInitiated, &reason))
    }
}

#[cfg(target_os = "macos")]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        unsafe { objc2_foundation::NSProcessInfo::processInfo().endActivity(&self.0) };
    }
}

#[cfg(not(target_os = "macos"))]
struct KeepAwake;

#[cfg(not(target_os = "macos"))]
impl KeepAwake {
    fn new() -> Self {
        KeepAwake
    }
}

/// この Mac (PC) の、家庭内ネットワークでの IPv4 アドレス。
fn local_ipv4() -> Vec<std::net::Ipv4Addr> {
    let mut ips: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(ip) if ip.is_private() => Some(ip),
            _ => None,
        })
        .collect();
    // 192.168.x.x (家庭用ルーターでよく使う) を先にする
    ips.sort_by_key(|ip| (ip.octets()[0] != 192, *ip));
    ips.dedup();
    ips
}

fn qr_svg(text: &str) -> Result<String> {
    let code = qrcode::QrCode::new(text.as_bytes())?;
    Ok(code.render::<qrcode::render::svg::Color>().min_dimensions(180, 180).quiet_zone(true).build())
}

// ---- エラー -------------------------------------------------------------

struct ApiError(String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, self.0).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(format!("{e:#}"))
    }
}

type ApiResult<T> = std::result::Result<Json<T>, ApiError>;

/// 重い処理を別スレッドで実行する。
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> ApiResult<T> {
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(r) => Ok(Json(r?)),
        Err(e) => Err(ApiError(e.to_string())),
    }
}

// ---- ライブラリ (本ごとのフォルダ) -----------------------------------

impl Ctx {
    /// パスがライブラリの中にあることを確かめる (ライブラリの外は読み書きさせない)。
    fn inside(&self, path: &str) -> Result<PathBuf> {
        let p = Path::new(path).canonicalize().with_context(|| format!("{path} が見つかりません"))?;
        if !p.starts_with(&self.library) {
            bail!("ライブラリ ({}) の外のファイルは使えません", self.library.display());
        }
        Ok(p)
    }

    fn book_dir(&self, name: &str) -> Result<PathBuf> {
        let name = name.trim();
        let bad = name.is_empty() || name.starts_with(['.', '_']) || name.contains(['/', '\\', ':']) || name.chars().any(|c| c.is_control());
        if bad {
            bail!("本の名前に使えない文字があります: {name}");
        }
        Ok(self.library.join(name))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Book {
    name: String,
    path: String,
    images: usize,
    /// 書き出した PDF があるか
    pdf: bool,
}

/// ライブラリの本の一覧 (新しい順)。
async fn books(State(ctx): State<Ctx>) -> ApiResult<Vec<Book>> {
    blocking(move || {
        let mut dirs: Vec<(std::time::SystemTime, Book)> = Vec::new();
        for e in std::fs::read_dir(&ctx.library)?.filter_map(|e| e.ok()) {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if !path.is_dir() || name.starts_with(['.', '_']) {
                continue;
            }
            let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            dirs.push((
                modified,
                Book {
                    images: book_core::input::list_images(&path).map(|v| v.len()).unwrap_or(0),
                    pdf: ctx.library.join(format!("{name}.pdf")).is_file(),
                    path: path.display().to_string(),
                    name,
                },
            ));
        }
        dirs.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(dirs.into_iter().map(|(_, b)| b).collect())
    })
    .await
}

#[derive(Deserialize)]
struct NameArgs {
    name: String,
}

/// 本 (フォルダ) を作り、そのパスを返す。すでにあればそのまま使う。
async fn create_book(State(ctx): State<Ctx>, Json(a): Json<NameArgs>) -> ApiResult<String> {
    let dir = ctx.book_dir(&a.name)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("{} を作れません", dir.display()))?;
    Ok(Json(dir.display().to_string()))
}

/// 写真を本のフォルダに追加する。送られた順にページが並ぶよう、連番のファイル名で保存する。
async fn upload(State(ctx): State<Ctx>, UrlPath(book): UrlPath<String>, mut form: Multipart) -> ApiResult<usize> {
    let dir = ctx.book_dir(&book)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("{} を作れません", dir.display()))?;
    let mut next = next_number(&dir);
    let mut saved = 0;
    while let Some(field) = form.next_field().await.map_err(|e| ApiError(e.to_string()))? {
        let ext = field
            .file_name()
            .and_then(|n| Path::new(n).extension())
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !book_core::input::SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let data = field.bytes().await.map_err(|e| ApiError(e.to_string()))?;
        let path = dir.join(format!("{next:04}.{ext}"));
        tokio::fs::write(&path, &data).await.map_err(|e| ApiError(format!("{}: {e}", path.display())))?;
        next += 1;
        saved += 1;
    }
    Ok(Json(saved))
}

/// 本のフォルダにある連番のファイル名 (0001.jpg など) の次の番号。
fn next_number(dir: &Path) -> u32 {
    book_core::input::list_images(dir)
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.file_stem()?.to_str()?.parse::<u32>().ok())
        .max()
        .map_or(1, |n| n + 1)
}

/// 書き出した PDF をブラウザに送る (Safari ではそのまま表示され、共有・保存できる)。
async fn download(State(ctx): State<Ctx>, UrlPath(book): UrlPath<String>) -> Result<Response, ApiError> {
    let dir = ctx.book_dir(&book)?;
    let pdf = ctx.library.join(format!("{}.pdf", dir.file_name().unwrap().to_string_lossy()));
    let data = tokio::fs::read(&pdf).await.map_err(|_| ApiError(format!("{} がありません", pdf.display())))?;
    // ファイル名は ASCII 以外も使えるよう RFC 5987 の形で渡す
    let encoded: String = book
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename*=UTF-8''{encoded}.pdf")),
        ],
        Body::from(data),
    )
        .into_response())
}

// ---- アプリと同じ処理 (ops) ---------------------------------------------

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

async fn open_folder(State(ctx): State<Ctx>, Json(a): Json<PathArgs>) -> ApiResult<ops::FolderInfo> {
    let dir = ctx.inside(&a.path)?;
    blocking(move || ops::open_folder(&dir.display().to_string())).await
}

#[derive(Deserialize)]
struct SaveOverrideArgs {
    folder: String,
    file: String,
    value: PageOverride,
}

async fn save_override(State(ctx): State<Ctx>, Json(a): Json<SaveOverrideArgs>) -> ApiResult<()> {
    let folder = ctx.inside(&a.folder)?;
    ctx.inside(&a.file)?;
    blocking(move || ops::save_override(&folder.display().to_string(), &a.file, a.value)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThumbnailArgs {
    path: String,
    max_side: u32,
    rotate: u16,
}

async fn thumbnail(State(ctx): State<Ctx>, Json(a): Json<ThumbnailArgs>) -> ApiResult<String> {
    ctx.inside(&a.path)?;
    blocking(move || ops::thumbnail(&a.path, a.max_side, a.rotate)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewArgs {
    path: String,
    settings: Settings,
    value: PageOverride,
    max_side: u32,
}

async fn preview(State(ctx): State<Ctx>, Json(a): Json<PreviewArgs>) -> ApiResult<ops::Preview> {
    ctx.inside(&a.path)?;
    blocking(move || ops::preview(&a.path, &a.settings, &a.value, a.max_side)).await
}

#[derive(Deserialize)]
struct ConvertArgs {
    input: String,
    settings: Settings,
}

/// 本を PDF に書き出す。保存先はライブラリの `<本の名前>.pdf` に決まっている。進捗は /api/events で送る。
async fn convert(State(ctx): State<Ctx>, Json(a): Json<ConvertArgs>) -> ApiResult<String> {
    let input = ctx.inside(&a.input)?;
    let name = input.file_name().context("本のフォルダではありません")?.to_string_lossy().into_owned();
    let output = ctx.library.join(format!("{name}.pdf"));
    let cancel = ctx.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    let tx = ctx.progress.clone();
    blocking(move || {
        let progress = |p: Progress| {
            let _ = tx.send(p);
        };
        ops::convert(&input.display().to_string(), &output.display().to_string(), &a.settings, &progress, &cancel)?;
        Ok(name)
    })
    .await
}

async fn cancel_convert(State(ctx): State<Ctx>) -> Json<()> {
    ctx.cancel.store(true, Ordering::SeqCst);
    Json(())
}

async fn sharpen_available() -> Json<Option<String>> {
    Json(ops::sharpen_available())
}

/// 書き出しの進捗 (Server-Sent Events)。
async fn events(State(ctx): State<Ctx>) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, std::convert::Infallible>>> {
    let stream = BroadcastStream::new(ctx.progress.subscribe())
        .filter_map(|p| p.ok())
        .map(|p| Ok(Event::default().json_data(p).unwrap_or_default()));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---- 画面のファイル --------------------------------------------------------

async fn asset(State(ctx): State<Ctx>, uri: Uri) -> Response {
    let resolver = ctx.app.asset_resolver();
    // 見つからないパスは index.html を返す (画面は 1 ページのアプリ)
    match resolver.get(uri.path().to_string()).or_else(|| resolver.get("index.html".into())) {
        Some(a) => ([(header::CONTENT_TYPE, a.mime_type)], a.bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
