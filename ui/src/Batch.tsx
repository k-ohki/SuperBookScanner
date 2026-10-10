// まとめて書き出す画面。複数の本 (画像のフォルダ) を、今の設定で 1 冊ずつ順に PDF にする。
// アプリではフォルダを選んで (親フォルダを選ぶと、その下の画像のあるフォルダをすべて) 追加する。
// ブラウザ (iPad など) では、Mac のライブラリの本から選ぶ。
import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import * as api from "./api";
import type { Settings } from "./api";

const isWindows = navigator.userAgent.includes("Windows");

type Status =
  | { kind: "waiting" }
  | { kind: "running"; label: string; fraction: number }
  // output はアプリでは PDF のパス、ブラウザでは本の名前 (ダウンロード用)
  | { kind: "done"; output: string }
  | { kind: "skipped" }
  | { kind: "cancelled" }
  | { kind: "error"; message: string };

type Item = api.BookFolder & {
  status: Status;
  /// ブラウザ: 書き出す本として選んだか
  checked: boolean;
  /// ブラウザ: 書き出した PDF がライブラリにあるか
  pdf: boolean;
};

type Dest = "beside" | "folder";

// 保存先の選び方は次に開いたときも使う (保存できなくても動く)
const load = (key: string) => {
  try {
    return localStorage.getItem(`batch.${key}`);
  } catch {
    return null;
  }
};
const store = (key: string, value: string) => {
  try {
    localStorage.setItem(`batch.${key}`, value);
  } catch {
    /* 保存できなくてもよい */
  }
};

const parentOf = (path: string) => path.replace(/[/\\][^/\\]*$/, "");
const sepOf = (path: string) => (path.includes("\\") ? "\\" : "/");

export default function BatchPanel(props: {
  settings: Settings;
  /// アプリ: 追加するフォルダ (ドロップされたものなど)。seq が変わるたびに追加する
  incoming: { paths: string[]; seq: number } | null;
  onClose: () => void;
  /// その本を開いて、画像ごとの調整をする
  onOpen: (path: string) => void;
}) {
  const [items, setItems] = useState<Item[]>([]);
  // まだ選んだことがなければ、1 冊ずつの書き出しと同じ保存先 (Windows は D:\MyProgram\SuperBookScanner\PDF) にまとめる
  const [dest, setDest] = useState<Dest>(() => {
    const d = load("dest");
    return d === "folder" || d === "beside" ? d : api.loadOutputDir() ? "folder" : "beside";
  });
  const [destDir, setDestDir] = useState<string>(() => load("destDir") ?? api.loadOutputDir());
  const [skipExisting, setSkipExisting] = useState(load("skipExisting") !== "false");
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // ブラウザ: ライブラリの本を読み込み中
  const [loading, setLoading] = useState(api.isRemote);
  const cancelled = useRef(false);

  const add = async (paths: string[]) => {
    setError(null);
    try {
      const found = await api.findBooks(paths);
      if (found.length === 0) setError("画像のあるフォルダが見つかりません。");
      setItems((cur) => [
        ...cur,
        ...found.filter((b) => !cur.some((c) => c.path === b.path)).map((b) => ({ ...b, status: { kind: "waiting" } as Status, checked: true, pdf: false })),
      ]);
    } catch (e) {
      setError(String(e));
    }
  };

  // アプリ: 渡されたフォルダを追加する
  useEffect(() => {
    if (props.incoming && props.incoming.paths.length > 0) add(props.incoming.paths);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.incoming?.seq]);

  // ブラウザ: ライブラリの本を並べる。PDF がまだない本を最初から選んでおく
  useEffect(() => {
    if (!api.isRemote) return;
    api.books().then(
      (books) =>
        setItems(
          books
            .filter((b) => b.images > 0)
            .map((b) => ({ path: b.path, name: b.name, images: b.images, pdf: b.pdf, checked: !b.pdf, status: { kind: "waiting" } as Status })),
        ),
      (e) => setError(String(e)),
    ).finally(() => setLoading(false));
  }, []);

  const chooseFolders = async () => {
    const picked = await open({ directory: true, multiple: true, title: "本のフォルダを選ぶ (親フォルダを選ぶと、その下をまとめて追加)" });
    if (Array.isArray(picked)) add(picked);
    else if (typeof picked === "string") add([picked]);
  };

  const chooseDestDir = async () => {
    const picked = await open({ directory: true, multiple: false, title: "PDF の保存先を選ぶ" });
    if (typeof picked === "string") {
      setDestDir(picked);
      store("destDir", picked);
    }
  };

  const update = (path: string, status: Status) => setItems((cur) => cur.map((i) => (i.path === path ? { ...i, status } : i)));

  const targets = items.filter((i) => (api.isRemote ? i.checked : true) && i.status.kind !== "done");
  const canRun = targets.length > 0 && !running && (api.isRemote || dest === "beside" || !!destDir);

  const run = async () => {
    setRunning(true);
    setError(null);
    cancelled.current = false;

    // 保存先。1 つのフォルダにまとめるときは、同じ名前の本に (2) などを付ける
    const used = new Set<string>();
    const outputs = targets.map((t) => {
      if (api.isRemote) return "";
      if (dest === "beside") return `${parentOf(t.path)}${sepOf(t.path)}${t.name}.pdf`;
      let name = t.name;
      for (let n = 2; used.has(name.toLowerCase()); n++) name = `${t.name} (${n})`;
      used.add(name.toLowerCase());
      return `${destDir}${sepOf(destDir)}${name}.pdf`;
    });
    let exists: boolean[] = targets.map((t) => t.pdf);
    if (!api.isRemote) {
      if (dest === "folder") {
        // まとめる保存先がまだなければ作る
        try {
          await api.ensureDir(destDir);
        } catch (e) {
          setError(String(e));
          setRunning(false);
          return;
        }
      }
      try {
        exists = await api.filesExist(outputs);
      } catch {
        exists = targets.map(() => false);
      }
    }
    for (const t of targets) update(t.path, { kind: "waiting" });

    let current: string | null = null;
    const unlisten = await api.onProgress((p) => {
      const d = api.describeProgress(p, props.settings.sharpen);
      if (d && current) update(current, { kind: "running", ...d });
    });
    try {
      for (let k = 0; k < targets.length; k++) {
        const t = targets[k];
        if (cancelled.current) break;
        if (skipExisting && exists[k]) {
          update(t.path, { kind: "skipped" });
          continue;
        }
        current = t.path;
        update(t.path, { kind: "running", label: "準備中", fraction: 0 });
        try {
          let output = outputs[k];
          if (api.isRemote) output = await api.convertRemote(t.path, props.settings);
          else await api.convert(t.path, output, props.settings);
          update(t.path, { kind: "done", output });
          if (api.isRemote) setItems((cur) => cur.map((i) => (i.path === t.path ? { ...i, pdf: true } : i)));
        } catch (e) {
          if (cancelled.current || String(e).includes("cancelled")) {
            update(t.path, { kind: "cancelled" });
            break;
          }
          // 1 冊が失敗しても、残りは続ける
          update(t.path, { kind: "error", message: String(e) });
        } finally {
          current = null;
        }
      }
    } finally {
      unlisten();
      setRunning(false);
    }
  };

  const cancel = () => {
    cancelled.current = true;
    api.cancelConvert();
  };

  const count = (kind: Status["kind"]) => items.filter((i) => i.status.kind === kind).length;
  const finished = count("done") + count("skipped") + count("error") > 0 && !running;

  return (
    <div className="overlay">
      <div className="dialog batch">
        <h3>まとめて書き出す</h3>
        <p className="hint">
          左の設定で、1 冊ずつ順に PDF にします。{api.isRemote ? "本" : "フォルダ"}ごとに行った画像の調整 (回転・分割位置など) もそのまま使います。
        </p>

        {!api.isRemote && (
          <div className="buttons start">
            <button onClick={chooseFolders} disabled={running}>
              フォルダを追加
            </button>
            {items.length > 0 && (
              <button className="link" onClick={() => setItems([])} disabled={running}>
                すべて外す
              </button>
            )}
          </div>
        )}

        <ul className="batch-list">
          {items.map((i) => (
            <li key={i.path} className={i.status.kind}>
              {api.isRemote && (
                <input
                  type="checkbox"
                  checked={i.checked}
                  disabled={running}
                  onChange={(e) => setItems((cur) => cur.map((c) => (c.path === i.path ? { ...c, checked: e.target.checked } : c)))}
                />
              )}
              <div className="what">
                <span className="name" title={i.path}>
                  {i.name}
                </span>
                <span className="hint">
                  {i.images} 枚{api.isRemote && i.pdf && i.status.kind !== "done" ? " · PDF あり" : ""}
                  {!api.isRemote && ` · ${parentOf(i.path).split(/[/\\]/).pop()} の中`}
                </span>
                {i.status.kind === "error" && <span className="error message">{i.status.message}</span>}
              </div>
              <ItemStatus item={i} />
              {!running && i.status.kind !== "running" && (
                <button className="link" onClick={() => props.onOpen(i.path)} title="開いて画像ごとに調整する">
                  開く
                </button>
              )}
              {!api.isRemote && !running && (
                <button className="link" onClick={() => setItems((cur) => cur.filter((c) => c.path !== i.path))} title="一覧から外す">
                  ×
                </button>
              )}
            </li>
          ))}
          {loading && <li className="hint">読み込み中…</li>}
          {items.length === 0 && !loading && (
            <li className="hint">
              {api.isRemote
                ? "画像のある本がまだありません。"
                : "「フォルダを追加」で本のフォルダを選ぶか、ここにドロップしてください。本のフォルダが入った親フォルダを選ぶと、その下をまとめて追加します。"}
            </li>
          )}
        </ul>

        {!api.isRemote && (
          <div className="dest">
            <label className="row check">
              <input type="radio" name="dest" checked={dest === "beside"} disabled={running} onChange={() => (setDest("beside"), store("dest", "beside"))} />
              各フォルダの隣に「フォルダ名.pdf」で保存
            </label>
            <label className="row check">
              <input type="radio" name="dest" checked={dest === "folder"} disabled={running} onChange={() => (setDest("folder"), store("dest", "folder"))} />
              1 つのフォルダにまとめて保存
            </label>
            {dest === "folder" && (
              <div className="row sub">
                <span className="path">{destDir || "未選択"}</span>
                <button onClick={chooseDestDir} disabled={running}>
                  選ぶ
                </button>
              </div>
            )}
          </div>
        )}
        <label className="row check">
          <input
            type="checkbox"
            checked={skipExisting}
            disabled={running}
            onChange={(e) => {
              setSkipExisting(e.target.checked);
              store("skipExisting", String(e.target.checked));
            }}
          />
          PDF がすでにある本はとばす
        </label>

        {error && <p className="error">{error}</p>}
        {finished && (
          <p>
            完了 {count("done")} 冊{count("skipped") > 0 && ` · とばした ${count("skipped")} 冊`}
            {count("error") > 0 && <span className="error"> · 失敗 {count("error")} 冊</span>}
          </p>
        )}

        <div className="buttons">
          {running ? (
            <button onClick={cancel}>中止</button>
          ) : (
            <>
              <button onClick={props.onClose}>閉じる</button>
              <button className="primary" onClick={run} disabled={!canRun}>
                {targets.length > 0 ? `${targets.length} 冊を書き出す` : "書き出す"}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function ItemStatus({ item }: { item: Item }) {
  const s = item.status;
  switch (s.kind) {
    case "waiting":
      return <span className="status" />;
    case "running":
      return (
        <span className="status">
          <span className="hint">{s.label}</span>
          <progress value={s.fraction} max={1} />
        </span>
      );
    case "done":
      return (
        <span className="status">
          {api.isRemote ? (
            <a className="button" href={api.downloadUrl(s.output)} target="_blank" rel="noreferrer">
              PDF
            </a>
          ) : (
            <button className="link" onClick={() => revealItemInDir(s.output)} title={s.output}>
              {isWindows ? "完了 · 表示" : "完了 · Finder で表示"}
            </button>
          )}
        </span>
      );
    case "skipped":
      return <span className="status hint">PDF あり · とばした</span>;
    case "cancelled":
      return <span className="status hint">中止</span>;
    case "error":
      return (
<span className="status error">失敗</span>
      );
  }
}
