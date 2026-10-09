// Rust 側 (src-tauri/src/lib.rs) のコマンドの型付きラッパー
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

export type Settings = {
  rotate: 0 | 90 | 180 | 270;
  unwarp: boolean;
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

export const openFolder = (path: string) => invoke<FolderInfo>("open_folder", { path });

export const thumbnail = (path: string, maxSide: number, rotate: number) => invoke<string>("thumbnail", { path, maxSide, rotate });

export const preview = (path: string, settings: Settings, value: PageOverride, maxSide: number) => invoke<Preview>("preview", { path, settings, value, maxSide });

export const saveOverride = (folder: string, file: string, value: PageOverride) => invoke<void>("save_override", { folder, file, value });

/// 空の項目を落とす (すべて空なら {} になり、保存ファイルから消える)
export const cleanOverride = (o: PageOverride): PageOverride => {
  const r: PageOverride = {};
  if (o.skip) r.skip = true;
  if (o.rotate != null) r.rotate = o.rotate;
  if (o.unwarp != null) r.unwarp = o.unwarp;
  if (o.dewarp != null) r.dewarp = o.dewarp;
  if (o.split != null) r.split = o.split;
  if (o.content && Object.keys(o.content).length > 0) r.content = o.content;
  return r;
};

export const isOverridden = (o: PageOverride | undefined) => !!o && Object.keys(cleanOverride(o)).length > 0;

export const convert = (input: string, output: string, settings: Settings) => invoke<void>("convert", { input, output, settings });

export const cancelConvert = () => invoke<void>("cancel_convert");

export const sharpenAvailable = () => invoke<string | null>("sharpen_available");

export const onProgress = (cb: (p: Progress) => void): Promise<UnlistenFn> => listen<Progress>("convert-progress", (e) => cb(e.payload));

export const initialFolder = () => invoke<string | null>("initial_folder");
