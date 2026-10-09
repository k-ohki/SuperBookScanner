import { useCallback, useEffect, useRef, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import * as api from "./api";
import type { FolderInfo, PreviewPage, Progress, Settings } from "./api";

type ExportState =
  | { phase: "idle" }
  | { phase: "running"; label: string; fraction: number }
  | { phase: "done"; output: string }
  | { phase: "error"; message: string };

export default function App() {
  const [folder, setFolder] = useState<FolderInfo | null>(null);
  const [settings, setSettings] = useState<Settings>(api.defaultSettings);
  const [selected, setSelected] = useState(0);
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const [pages, setPages] = useState<PreviewPage[] | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [sharpenPath, setSharpenPath] = useState<string | null>(null);
  const [exportState, setExportState] = useState<ExportState>({ phase: "idle" });

  useEffect(() => {
    api.sharpenAvailable().then(setSharpenPath);
  }, []);

  const loadFolder = useCallback(async (path: string) => {
    try {
      const info = await api.openFolder(path);
      setFolder(info);
      setSelected(0);
      setThumbs({});
      setPages(null);
      setExportState({ phase: "idle" });
    } catch (e) {
      alert(String(e));
    }
  }, []);

  // 起動時にフォルダが渡されていれば開く
  useEffect(() => {
    api.initialFolder().then((p) => {
      if (p) loadFolder(p);
    });
  }, [loadFolder]);

  // フォルダのドラッグ & ドロップ
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type === "drop" && e.payload.paths.length > 0) loadFolder(e.payload.paths[0]);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [loadFolder]);

  // サムネイル (回転を変えたら作り直す)。3 枚ずつ並行して作る
  useEffect(() => {
    if (!folder) return;
    let cancelled = false;
    setThumbs({});
    const queue = [...folder.files];
    const worker = async () => {
      while (!cancelled && queue.length > 0) {
        const file = queue.shift()!;
        try {
          const url = await api.thumbnail(file, 240, settings.rotate);
          if (!cancelled) setThumbs((t) => ({ ...t, [file]: url }));
        } catch {
          /* 読めない画像はサムネイルなし */
        }
      }
    };
    for (let i = 0; i < 3; i++) worker();
    return () => {
      cancelled = true;
    };
  }, [folder, settings.rotate]);

  // 選んだ画像のプレビュー (設定を変えたら少し待ってから作り直す)
  const previewSeq = useRef(0);
  useEffect(() => {
    if (!folder || folder.files.length === 0) return;
    const seq = ++previewSeq.current;
    const timer = setTimeout(async () => {
      setPreviewing(true);
      setPreviewError(null);
      try {
        // プレビューでは鮮明化はしない (時間がかかるため。書き出し時に行う)
        const result = await api.preview(folder.files[selected], { ...settings, sharpen: false }, 1600);
        if (seq === previewSeq.current) setPages(result);
      } catch (e) {
        if (seq === previewSeq.current) setPreviewError(String(e));
      } finally {
        if (seq === previewSeq.current) setPreviewing(false);
      }
    }, 250);
    return () => clearTimeout(timer);
  }, [folder, selected, settings]);

  const chooseFolder = async () => {
    const path = await open({ directory: true, multiple: false, title: "ページ画像のフォルダを選ぶ" });
    if (typeof path === "string") loadFolder(path);
  };

  const startExport = async () => {
    if (!folder) return;
    const parent = folder.path.replace(/[/\\][^/\\]*$/, "");
    const output = await save({ defaultPath: `${parent}/${folder.name}.pdf`, filters: [{ name: "PDF", extensions: ["pdf"] }] });
    if (!output) return;

    setExportState({ phase: "running", label: "準備中", fraction: 0 });
    const unlisten = await api.onProgress((p: Progress) => {
      if (p.kind === "PageProcessed") setExportState({ phase: "running", label: `補正 ${p.done} / ${p.total}`, fraction: (p.done / p.total) * (settings.sharpen ? 0.4 : 0.8) });
      if (p.kind === "PageSharpened") setExportState({ phase: "running", label: `AI 鮮明化 ${p.done} / ${p.total}`, fraction: 0.4 + (p.done / p.total) * 0.45 });
      if (p.kind === "PageEncoded") setExportState({ phase: "running", label: `PDF 作成 ${p.done} / ${p.total}`, fraction: (settings.sharpen ? 0.85 : 0.8) + (p.done / p.total) * 0.15 });
    });
    try {
      await api.convert(folder.path, output, settings);
      setExportState({ phase: "done", output });
    } catch (e) {
      setExportState(String(e).includes("cancelled") ? { phase: "idle" } : { phase: "error", message: String(e) });
    } finally {
      unlisten();
    }
  };

  const set = <K extends keyof Settings>(key: K, value: Settings[K]) => setSettings((s) => ({ ...s, [key]: value }));
  const running = exportState.phase === "running";

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">SuperBookScanner</div>
        <button onClick={chooseFolder} disabled={running}>
          フォルダを開く
        </button>
        {folder && (
          <div className="folder" title={folder.path}>
            {folder.name} · {folder.files.length} 枚
          </div>
        )}
        <div className="spacer" />
        {folder && (
          <button className="primary" onClick={startExport} disabled={running || folder.files.length === 0}>
            PDF に書き出す
          </button>
        )}
      </header>

      <div className="body">
        <aside className="settings">
          <h3>入力</h3>
          <label className="row">
            回転
            <select value={settings.rotate} onChange={(e) => set("rotate", Number(e.target.value) as Settings["rotate"])}>
              <option value={0}>なし</option>
              <option value={90}>右に 90°</option>
              <option value={180}>180°</option>
              <option value={270}>左に 90°</option>
            </select>
          </label>
          <Check label="見開きを左右に分ける" value={settings.split} onChange={(v) => set("split", v)} />

          <h3>補正</h3>
          <Check label="傾き補正" value={settings.deskew} onChange={(v) => set("deskew", v)} />
          <Check label="歪み補正 (ノドの湾曲)" value={settings.dewarp} onChange={(v) => set("dewarp", v)} />
          <Check label="影・照明ムラの除去" value={settings.illumination} onChange={(v) => set("illumination", v)} />
          <Check label="余白をそろえる" value={settings.crop} onChange={(v) => set("crop", v)} />
          {settings.crop && (
            <label className="row sub">
              余白 {Math.round(settings.margin * 100)}%
              <input type="range" min={0} max={0.15} step={0.01} value={settings.margin} onChange={(e) => set("margin", Number(e.target.value))} />
            </label>
          )}

          <h3>AI 鮮明化</h3>
          <Check label="Real-ESRGAN で鮮明にする" value={settings.sharpen} disabled={!sharpenPath} onChange={(v) => set("sharpen", v)} />
          {!sharpenPath && <p className="hint">realesrgan-ncnn-vulkan が見つかりません。mac/scripts/fetch-realesrgan.sh を実行してください。</p>}
          {settings.sharpen && (
            <label className="row sub">
              解像度
              <select value={settings.sharpenScale} onChange={(e) => set("sharpenScale", Number(e.target.value))}>
                <option value={1}>300dpi 相当</option>
                <option value={2}>600dpi 相当</option>
              </select>
            </label>
          )}

          <h3>PDF</h3>
          <Check label="右綴じ (縦書きの本)" value={settings.rtl} onChange={(v) => set("rtl", v)} />
          <label className="row">
            画質 {settings.quality}
            <input type="range" min={50} max={100} step={1} value={settings.quality} onChange={(e) => set("quality", Number(e.target.value))} />
          </label>
        </aside>

        <main className="preview">
          {!folder && (
            <div className="empty">
              <p>ページ画像の入ったフォルダを、ここにドロップするか「フォルダを開く」で選んでください。</p>
              <p className="hint">1 フォルダ = 1 冊。ファイル名の順 (2 → 10 の順) にページを並べます。</p>
            </div>
          )}
          {folder && (
            <div className="compare">
              <figure>
                <figcaption>元の画像</figcaption>
                {thumbs[folder.files[selected]] ? <img src={thumbs[folder.files[selected]]} alt="" /> : <div className="placeholder" />}
              </figure>
              <figure className={previewing ? "busy" : ""}>
                <figcaption>補正後 {previewing && "(処理中…)"}</figcaption>
                {previewError && <p className="error">{previewError}</p>}
                <div className="pages">
                  {pages?.map((p, i) => (
                    <PageView key={i} page={p} showBox={settings.crop} />
                  ))}
                </div>
                {pages && <PageInfo pages={pages} />}
              </figure>
            </div>
          )}
        </main>
      </div>

      {folder && (
        <footer className="filmstrip">
          {folder.files.map((f, i) => (
            <button key={f} className={`thumb ${i === selected ? "selected" : ""}`} onClick={() => setSelected(i)} title={f}>
              {thumbs[f] ? <img src={thumbs[f]} alt="" /> : <div className="placeholder" />}
              <span>{i + 1}</span>
            </button>
          ))}
        </footer>
      )}

      {exportState.phase !== "idle" && (
        <div className="overlay">
          <div className="dialog">
            {exportState.phase === "running" && (
              <>
                <p>{exportState.label}</p>
                <progress value={exportState.fraction} max={1} />
                <button onClick={() => api.cancelConvert()}>中止</button>
              </>
            )}
            {exportState.phase === "done" && (
              <>
                <p>書き出しました</p>
                <p className="path">{exportState.output}</p>
                <div className="buttons">
                  <button onClick={() => revealItemInDir(exportState.output)}>Finder で表示</button>
                  <button className="primary" onClick={() => setExportState({ phase: "idle" })}>
                    閉じる
                  </button>
                </div>
              </>
            )}
            {exportState.phase === "error" && (
              <>
                <p>書き出しに失敗しました</p>
                <pre className="error">{exportState.message}</pre>
                <button onClick={() => setExportState({ phase: "idle" })}>閉じる</button>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function Check(props: { label: string; value: boolean; disabled?: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className={`row check ${props.disabled ? "disabled" : ""}`}>
      <input type="checkbox" checked={props.value} disabled={props.disabled} onChange={(e) => props.onChange(e.target.checked)} />
      {props.label}
    </label>
  );
}

// 補正後のページ。本文の範囲 (余白をそろえる基準) を枠で示す
function PageView({ page, showBox }: { page: PreviewPage; showBox: boolean }) {
  const b = page.report.content_box;
  return (
    <div className="page">
      <img src={page.image} alt="" />
      {showBox && b && (
        <div
          className="box"
          style={{
            left: `${(b.x / page.width) * 100}%`,
            top: `${(b.y / page.height) * 100}%`,
            width: `${(b.w / page.width) * 100}%`,
            height: `${(b.h / page.height) * 100}%`,
          }}
        />
      )}
    </div>
  );
}

function PageInfo({ pages }: { pages: PreviewPage[] }) {
  return (
    <ul className="info">
      {pages.map((p, i) => {
        const r = p.report;
        const side = pages.length > 1 ? (i === 0 ? "左: " : "右: ") : "";
        const skew = r.deskew ? `傾き ${r.deskew.angle_deg.toFixed(2)}°` : "傾き補正なし";
        const warp = r.dewarp ? (r.dewarp.applied ? `歪み ${Math.round(r.dewarp.max_displacement_px)}px を補正` : "歪み補正なし (行が見つからない)") : "歪み補正オフ";
        return (
          <li key={i}>
            {side}
            {skew} · {warp}
          </li>
        );
      })}
    </ul>
  );
}
