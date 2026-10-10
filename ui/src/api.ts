// Rust 側 (crates/book-app) のコマンドの型付きラッパー。
// アプリの中では Tauri の invoke で、iPad などのブラウザからは Mac の Web サーバー (remote.rs) に HTTP で呼ぶ。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/// ブラウザから使っている (アプリの外) か
export const isRemote = !("__TAURI_INTERNALS__" in window);

async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!isRemote) return invoke<T>(cmd, args);
  const res = await fetch(`/api/${cmd}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(args) });
  if (!res.ok) throw new Error(await res.text());
  return (await res.json()) as T;
}

export type Settings = {
  rotate: 0 | 90 | 180 | 270;
  unwarp: boolean;
  /// 平面化で紙の反りも直す (false なら台形だけ。影で文字が曲がるとき用)
  unwarpCurl: boolean;
  split: boolean;
  deskew: boolean;
  dewarp: boolean;
  illumination: boolean;
  crop: boolean;
  margin: number;
  quality: number;
  rtl: boolean;
  sharpen: boolean;
  sharpenScale: number;
};

export const defaultSettings: Settings = {
  rotate: 0,
  unwarp: true,
  unwarpCurl: true,
  split: true,
  deskew: true,
  dewarp: true,
  illumination: true,
  crop: true,
  margin: 0.05,
  quality: 85,
  rtl: false,
  sharpen: false,
  sharpenScale: 2,
};

export type SplitOverride = "none" | { at: number };

export type RectF = { x: number; y: number; w: number; h: number };

/// 1 枚の画像だけの手動調整 (Rust の project::PageOverride)。省略した項目は全体の設定・自動処理に従う
export type PageOverride = {
  skip?: boolean;
  rotate?: 0 | 90 | 180 | 270 | null;
  unwarp?: boolean | null;
  unwarpCurl?: boolean | null;
  dewarp?: boolean | null;
  split?: SplitOverride | null;
  content?: Record<string, RectF>;
};

export type FolderInfo = { path: string; name: string; files: string[]; overrides: PageOverride[] };

export type Rect = { x: number; y: number; w: number; h: number };

export type PageReport = {
  source: string;
  unwarp: { applied: boolean; message: string } | null;
  deskew: { angle_deg: number; applied: boolean } | null;
  dewarp: { applied: boolean; message: string; max_displacement_px: number } | null;
  content_box: Rect | null;
  split: { gutter_x: number | null };
  half: number | null;
};

export type PreviewPage = { image: string; width: number; height: number; report: PageReport };

export type Preview = { stage: string; stageWidth: number; stageHeight: number; pages: PreviewPage[] };

export type Progress =
  | { kind: "PageProcessed"; done: number; total: number; file: string }
  | { kind: "PageSharpened"; done: number; total: number }
  | { kind: "PageEncoded"; done: number; total: number }
  | { kind: "Finished"; output: string };

export const openFolder = (path: string) => call<FolderInfo>("open_folder", { path });

/// まとめて書き出すときの 1 冊 (画像のあるフォルダ)
export type BookFolder = { path: string; name: string; images: number };

/// 選んだフォルダとその下から、画像のあるフォルダをすべて探す (アプリだけ)
export const findBooks = (paths: string[]) => call<BookFolder[]>("find_books", { paths });

/// それぞれのファイルがすでにあるか (アプリだけ)
export const filesExist = (paths: string[]) => call<boolean[]>("files_exist", { paths });

export const thumbnail = (path: string, maxSide: number, rotate: number) => call<string>("thumbnail", { path, maxSide, rotate });

export const preview = (path: string, settings: Settings, value: PageOverride, maxSide: number) => call<Preview>("preview", { path, settings, value, maxSide });

export const saveOverride = (folder: string, file: string, value: PageOverride) => call<void>("save_override", { folder, file, value });

/// 空の項目を落とす (すべて空なら {} になり、保存ファイルから消える)
export const cleanOverride = (o: PageOverride): PageOverride => {
  const r: PageOverride = {};
  if (o.skip) r.skip = true;
  if (o.rotate != null) r.rotate = o.rotate;
  if (o.unwarp != null) r.unwarp = o.unwarp;
  if (o.unwarpCurl != null) r.unwarpCurl = o.unwarpCurl;
  if (o.dewarp != null) r.dewarp = o.dewarp;
  if (o.split != null) r.split = o.split;
  if (o.content && Object.keys(o.content).length > 0) r.content = o.content;
  return r;
};

export const isOverridden = (o: PageOverride | undefined) => !!o && Object.keys(cleanOverride(o)).length > 0;

export const convert = (input: string, output: string, settings: Settings) => call<void>("convert", { input, output, settings });

export const cancelConvert = () => call<void>("cancel_convert");

export const sharpenAvailable = () => call<string | null>("sharpen_available");

/// 書き出しの進捗を受け取る。戻り値は受け取りをやめる関数
export const onProgress = async (cb: (p: Progress) => void): Promise<() => void> => {
  if (!isRemote) return listen<Progress>("convert-progress", (e) => cb(e.payload));
  const es = new EventSource("/api/events");
  es.onmessage = (e) => cb(JSON.parse(e.data) as Progress);
  // つながるのを待ってから書き出しを始める (最初の進捗を取りこぼさないため)
  await new Promise<void>((resolve) => {
    es.onopen = () => resolve();
    setTimeout(resolve, 2000);
  });
  return () => es.close();
};

/// 進捗を表示用の文言と割合 (0..1) にする
export const describeProgress = (p: Progress, sharpen: boolean): { label: string; fraction: number } | null => {
  if (p.kind === "PageProcessed") return { label: `補正 ${p.done} / ${p.total}`, fraction: (p.done / p.total) * (sharpen ? 0.4 : 0.8) };
  if (p.kind === "PageSharpened") return { label: `AI 鮮明化 ${p.done} / ${p.total}`, fraction: 0.4 + (p.done / p.total) * 0.45 };
  if (p.kind === "PageEncoded") return { label: `PDF 作成 ${p.done} / ${p.total}`, fraction: (sharpen ? 0.85 : 0.8) + (p.done / p.total) * 0.15 };
  return null;
};

export const initialFolder = () => call<string | null>("initial_folder");

// ---- アプリだけ: iPad などから使う (Web サーバー) -------------------------

export type RemoteInfo = { urls: string[]; qrSvg: string; library: string };

export const remoteStart = () => call<RemoteInfo>("remote_start");

export const remoteStop = () => call<void>("remote_stop");

export const remoteStatus = () => call<RemoteInfo | null>("remote_status");

// ---- ブラウザだけ: ライブラリ (Mac の書類/SuperBookScanner) の本 ------------

export type Book = { name: string; path: string; images: number; pdf: boolean };

export const books = () => call<Book[]>("books");

export const createBook = (name: string) => call<string>("create_book", { name });

/// 写真を本に追加する (送った順にページが並ぶ)。戻り値は追加した枚数
export const uploadPhotos = async (book: string, files: FileList | File[], onProgress?: (done: number, total: number) => void): Promise<number> => {
  const list = Array.from(files);
  let saved = 0;
  // 1 枚ずつ送る (大きな写真をまとめて送ると途中で失敗しやすいため)
  for (let i = 0; i < list.length; i++) {
    const form = new FormData();
    form.append("file", list[i], list[i].name);
    const res = await fetch(`/api/upload/${encodeURIComponent(book)}`, { method: "POST", body: form });
    if (!res.ok) throw new Error(await res.text());
    saved += (await res.json()) as number;
    onProgress?.(i + 1, list.length);
  }
  return saved;
};

/// ブラウザでの書き出し。保存先は Mac のライブラリに決まっていて、本の名前を返す
export const convertRemote = (input: string, settings: Settings) => call<string>("convert", { input, settings });

export const downloadUrl = (book: string) => `/api/download/${encodeURIComponent(book)}`;
