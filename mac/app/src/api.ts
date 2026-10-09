// Rust 側 (src-tauri/src/lib.rs) のコマンドの型付きラッパー
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

export type Settings = {
  rotate: 0 | 90 | 180 | 270;
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

export type FolderInfo = { path: string; name: string; files: string[] };

export type Rect = { x: number; y: number; w: number; h: number };

export type PageReport = {
  source: string;
  deskew: { angle_deg: number; applied: boolean } | null;
  dewarp: { applied: boolean; message: string; max_displacement_px: number } | null;
  content_box: Rect | null;
  split: { gutter_x: number | null };
  half: number | null;
};

export type PreviewPage = { image: string; width: number; height: number; report: PageReport };

export type Progress =
  | { kind: "PageProcessed"; done: number; total: number; file: string }
  | { kind: "PageSharpened"; done: number; total: number }
  | { kind: "PageEncoded"; done: number; total: number }
  | { kind: "Finished"; output: string };

export const openFolder = (path: string) => invoke<FolderInfo>("open_folder", { path });

export const thumbnail = (path: string, maxSide: number, rotate: number) => invoke<string>("thumbnail", { path, maxSide, rotate });

export const preview = (path: string, settings: Settings, maxSide: number) => invoke<PreviewPage[]>("preview", { path, settings, maxSide });

export const convert = (input: string, output: string, settings: Settings) => invoke<void>("convert", { input, output, settings });

export const cancelConvert = () => invoke<void>("cancel_convert");

export const sharpenAvailable = () => invoke<string | null>("sharpen_available");

export const onProgress = (cb: (p: Progress) => void): Promise<UnlistenFn> => listen<Progress>("convert-progress", (e) => cb(e.payload));

export const initialFolder = () => invoke<string | null>("initial_folder");
