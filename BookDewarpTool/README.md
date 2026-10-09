# BookDewarpTool - 本の綴じ目 (ノド) 付近の湾曲補正

裁断せずにスキャン・撮影した本のページで、綴じ目 (ノド) 付近の行が曲がってしまう歪みを補正します。
補正本体は `SuperBookTools/Basic/BookDewarp.cs` にあり、本ツールと SuperBookToolsApp の両方から使われます。

## 使い方

単体ツール (Windows / Linux x64):

```
dotnet run -c Release --project BookDewarpTool -- <入力ファイル|ディレクトリ> <出力ファイル|ディレクトリ> [--debug] [--no-horizontal] [--strength=1.0]
```

- `--debug`: 検出した行 (赤) と補正後の直線 (緑) を描いた `*.dewarp_debug.png` を出力
- `--no-horizontal`: ノド付近の横方向の伸ばし補正を行わない
- `--strength=`: 横方向補正の強さ

SuperBookToolsApp から:

- `ConvertPdf [srcDir] /dst:dstDir /dewarp:yes` … PDF 変換の途中 (Real-ESRGAN の前) で補正する (既定は無効)
- `DewarpImages [srcDir] /dst:dstDir` … ディレクトリ内の画像だけを補正する

## アルゴリズム

ScanTailor の dewarping (テキスト行をトレースし、行の曲がりから面の歪みを推定して展開する) の考え方を参考に、独自に実装しています。ScanTailor のソースコードは使用していません。

1. 解析用に縮小した画像を 2 値化し (局所適応 2 値化と大津法の AND。ノドの影を文字と誤認しない)、文字サイズの連結成分だけを残す
2. 文字を横につないでテキスト行を作り、各行の中心線を多項式で近似する
3. 各行の「平らな部分」に頑健に直線を当てはめ、曲線との差を縦方向の変位とする
   (縦書きなどで横の行が取れない場合は、本文ブロックの上端・下端の包絡線を使う)
4. 複数行の変位を縦方向に補間し、ページ全体の変位場を作る (ScanTailor の上下 2 曲線モデルを多曲線に一般化)
5. ノド付近の奥行きによる横方向の縮みを、変位プロファイルの弧長で近似して伸ばす
6. `cv::remap` で展開する

行がほとんどない・歪みが小さいページは何もせずそのまま出力します。
