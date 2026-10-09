import { useCallback, useEffect, useRef, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import * as api from "./api";
import type { FolderInfo, PageOverride, Preview, PreviewPage, Progress, RectF, Settings } from "./api";

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
  const [overrides, setOverrides] = useState<PageOverride[]>([]);
  const [result, setResult] = useState<Preview | null>(null);
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
      setOverrides(info.overrides.map(api.cleanOverride));
      setSelected(0);
      setThumbs({});
      setResult(null);
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
  const ovr: PageOverride = overrides[selected] ?? {};
  const ovrKey = JSON.stringify(ovr);
  const previewSeq = useRef(0);
  useEffect(() => {
    if (!folder || folder.files.length === 0) return;
    const seq = ++previewSeq.current;
    const timer = setTimeout(async () => {
      setPreviewing(true);
      setPreviewError(null);
      try {
        // プレビューでは鮮明化はしない (時間がかかるため。書き出し時に行う)
        const r = await api.preview(folder.files[selected], { ...settings, sharpen: false }, JSON.parse(ovrKey), 1600);
        if (seq === previewSeq.current) setResult(r);
      } catch (e) {
        if (seq === previewSeq.current) setPreviewError(String(e));
      } finally {
        if (seq === previewSeq.current) setPreviewing(false);
      }
    }, 250);
    return () => clearTimeout(timer);
  }, [folder, selected, settings, ovrKey]);

  // この画像だけの調整を変えて保存する
  const setOvr = (patch: Partial<PageOverride>) => {
    if (!folder) return;
    const value = api.cleanOverride({ ...ovr, ...patch });
    setOverrides((all) => all.map((o, i) => (i === selected ? value : o)));
    api.saveOverride(folder.path, folder.files[selected], value).catch((e) => alert(String(e)));
  };
  const setContent = (half: number, rect: RectF | null) => {
    const content = { ...(ovr.content ?? {}) };
    if (rect) content[String(half)] = rect;
    else delete content[String(half)];
    setOvr({ content });
  };

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
          <Check label="写真の紙を平らにする (台形・反り・背景)" value={settings.unwarp} onChange={(v) => set("unwarp", v)} />
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

          {folder && (
            <>
              <h3>この画像だけ</h3>
              <label className="row">
                回転
                <select value={ovr.rotate ?? ""} onChange={(e) => setOvr({ rotate: e.target.value === "" ? null : (Number(e.target.value) as Settings["rotate"]) })}>
                  <option value="">全体の設定</option>
                  <option value={0}>なし</option>
                  <option value={90}>右に 90°</option>
                  <option value={180}>180°</option>
                  <option value={270}>左に 90°</option>
                </select>
              </label>
              <Tri label="平らにする" value={ovr.unwarp} onChange={(v) => setOvr({ unwarp: v })} />
              <Tri label="歪み補正" value={ovr.dewarp} onChange={(v) => setOvr({ dewarp: v })} />
              <label className="row">
                見開き
                <select
                  value={ovr.split == null ? "" : ovr.split === "none" ? "none" : "at"}
                  onChange={(e) => setOvr({ split: e.target.value === "" ? null : e.target.value === "none" ? "none" : { at: 0.5 } })}
                >
                  <option value="">自動</option>
                  <option value="none">分けない</option>
                  <option value="at">位置を指定</option>
                </select>
              </label>
              <Check label="この画像を書き出さない" value={!!ovr.skip} onChange={(v) => setOvr({ skip: v })} />
              {api.isOverridden(ovr) && (
                <button className="link" onClick={() => setOvr({ skip: false, rotate: null, unwarp: null, dewarp: null, split: null, content: {} })}>
                  調整を元に戻す
                </button>
              )}
              <p className="hint">左の画像の縦線をドラッグすると分割位置、右の青い枠をドラッグすると本文の範囲を直せます。</p>
            </>
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
                <figcaption>{result ? "分割前の画像" : "元の画像"}</figcaption>
                {result ? (
                  <StageView preview={result} split={ovr.split} onSplit={(at) => setOvr({ split: { at } })} />
                ) : thumbs[folder.files[selected]] ? (
                  <img src={thumbs[folder.files[selected]]} alt="" />
                ) : (
                  <div className="placeholder" />
                )}
              </figure>
              <figure className={previewing ? "busy" : ""}>
                <figcaption>
                  補正後 {previewing && "(処理中…)"} {ovr.skip && "· 書き出さない"}
                </figcaption>
                {previewError && <p className="error">{previewError}</p>}
                <div className="pages">
                  {result?.pages.map((p, i) => (
                    <PageView
                      key={i}
                      page={p}
                      showBox={settings.crop}
                      manual={!!ovr.content?.[String(i)]}
                      onBox={(r) => setContent(i, r)}
                      onReset={() => setContent(i, null)}
                    />
                  ))}
                </div>
                {result && <PageInfo pages={result.pages} />}
              </figure>
            </div>
          )}
        </main>
      </div>

      {folder && (
        <footer className="filmstrip">
          {folder.files.map((f, i) => (
            <button key={f} className={`thumb ${i === selected ? "selected" : ""} ${overrides[i]?.skip ? "skipped" : ""}`} onClick={() => setSelected(i)} title={f}>
              {thumbs[f] ? <img src={thumbs[f]} alt="" /> : <div className="placeholder" />}
              <span>
                {i + 1}
                {api.isOverridden(overrides[i]) && " ●"}
              </span>
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

// 3 択 (全体の設定 / する / しない)
function Tri(props: { label: string; value: boolean | null | undefined; onChange: (v: boolean | null) => void }) {
  return (
    <label className="row">
      {props.label}
      <select value={props.value == null ? "" : props.value ? "on" : "off"} onChange={(e) => props.onChange(e.target.value === "" ? null : e.target.value === "on")}>
        <option value="">全体の設定</option>
        <option value="on">する</option>
        <option value="off">しない</option>
      </select>
    </label>
  );
}

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));

// 要素内のポインタ位置 (0..1)
function relPos(el: HTMLElement, e: { clientX: number; clientY: number }) {
  const r = el.getBoundingClientRect();
  return { x: clamp01((e.clientX - r.left) / r.width), y: clamp01((e.clientY - r.top) / r.height) };
}

// 平面化した見開き。縦線 (分割位置) をドラッグで動かせる
function StageView({ preview, split, onSplit }: { preview: Preview; split: PageOverride["split"]; onSplit: (at: number) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<number | null>(null);
  const gutter = preview.pages[0]?.report.split.gutter_x;
  const at = drag ?? (gutter != null ? gutter / preview.stageWidth : null);
  const canSplit = split !== "none";

  const onDown = (e: React.PointerEvent) => {
    if (!canSplit || !ref.current) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    setDrag(relPos(ref.current, e).x);
  };
  const onMove = (e: React.PointerEvent) => {
    if (drag != null && ref.current) setDrag(relPos(ref.current, e).x);
  };
  const onUp = () => {
    if (drag != null) onSplit(Math.round(drag * 1000) / 1000);
    setDrag(null);
  };

  return (
    <div ref={ref} className={`stage ${canSplit ? "splittable" : ""}`} onPointerDown={onDown} onPointerMove={onMove} onPointerUp={onUp}>
      <img src={preview.stage} alt="" draggable={false} />
      {at != null && <div className="split-line" style={{ left: `${at * 100}%` }} />}
    </div>
  );
}

type Handle = "move" | "nw" | "ne" | "sw" | "se";

// 補正後のページ。本文の範囲 (余白をそろえる基準) を枠で示し、ドラッグで直せる
function PageView(props: { page: PreviewPage; showBox: boolean; manual: boolean; onBox: (r: RectF) => void; onReset: () => void }) {
  const { page } = props;
  const ref = useRef<HTMLDivElement>(null);
  const b = page.report.content_box;
  const auto: RectF | null = b ? { x: b.x / page.width, y: b.y / page.height, w: b.w / page.width, h: b.h / page.height } : null;
  const [drag, setDrag] = useState<{ handle: Handle; start: { x: number; y: number }; rect: RectF } | null>(null);
  const [live, setLive] = useState<RectF | null>(null);
  const rect = live ?? auto;

  const begin = (handle: Handle) => (e: React.PointerEvent) => {
    if (!rect || !ref.current) return;
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    setDrag({ handle, start: relPos(ref.current, e), rect });
  };
  const move = (e: React.PointerEvent) => {
    if (!drag || !ref.current) return;
    const p = relPos(ref.current, e);
    const dx = p.x - drag.start.x;
    const dy = p.y - drag.start.y;
    const r = drag.rect;
    let x0 = r.x, y0 = r.y, x1 = r.x + r.w, y1 = r.y + r.h;
    if (drag.handle === "move") {
      const mx = Math.min(Math.max(dx, -x0), 1 - x1);
      const my = Math.min(Math.max(dy, -y0), 1 - y1);
      x0 += mx; x1 += mx; y0 += my; y1 += my;
    } else {
      if (drag.handle.includes("w")) x0 = clamp01(Math.min(x0 + dx, x1 - 0.02));
      if (drag.handle.includes("e")) x1 = clamp01(Math.max(x1 + dx, x0 + 0.02));
      if (drag.handle.includes("n")) y0 = clamp01(Math.min(y0 + dy, y1 - 0.02));
      if (drag.handle.includes("s")) y1 = clamp01(Math.max(y1 + dy, y0 + 0.02));
    }
    setLive({ x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
  };
  const end = () => {
    if (drag && live) props.onBox(live);
    setDrag(null);
  };
  // 新しいプレビューが届いたら、ドラッグ中の仮の枠を捨てる
  useEffect(() => setLive(null), [page]);

  return (
    <div className="page" ref={ref} onPointerMove={move} onPointerUp={end}>
      <img src={page.image} alt="" draggable={false} />
      {props.showBox && rect && (
        <div
          className={`box ${props.manual ? "manual" : ""}`}
          style={{ left: `${rect.x * 100}%`, top: `${rect.y * 100}%`, width: `${rect.w * 100}%`, height: `${rect.h * 100}%` }}
          onPointerDown={begin("move")}
        >
          {(["nw", "ne", "sw", "se"] as Handle[]).map((h) => (
            <span key={h} className={`handle ${h}`} onPointerDown={begin(h)} />
          ))}
          {props.manual && (
            <button className="reset" onPointerDown={(e) => e.stopPropagation()} onClick={props.onReset} title="自動の範囲に戻す">
              自動
            </button>
          )}
        </div>
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
        const flat = r.unwarp && !r.unwarp.applied ? `平面化できず (${r.unwarp.message}) · ` : "";
        return (
          <li key={i}>
            {side}
            {flat}
            {skew} · {warp}
          </li>
        );
      })}
    </ul>
  );
}
