// 本の綴じ目 (ノド) 付近の湾曲 (行の曲がり) を補正するモジュール
//
// ScanTailor の dewarping (テキスト行のトレース → 歪みモデル → 面の展開) の考え方を参考に、
// 独自に実装したものである (ScanTailor のソースコードは移植していない)。
//
// 処理の流れ:
//   1. 解析用に縮小したグレー画像を 2 値化し、文字らしい連結成分だけを残す
//   2. 文字を横方向に連結してテキスト行を作り、各行の中心線を多項式で近似する
//   3. 各行について「平らな部分」に頑健に直線を当てはめ、曲線と直線の差を縦方向の変位とする
//      (縦書きなどで行が取れない場合は、本文ブロックの上端・下端の包絡線を代わりに使う)
//   4. 複数の行の変位を y 方向に補間して、ページ全体の変位場 D(x, y) を作る
//   5. ノド付近の奥行きによる横方向の縮みを、変位プロファイルの弧長で近似して伸ばす
//   6. cv::remap で画像を展開する
//
// OpenCvSharp と .NET 標準ライブラリだけに依存するので、単体ツール (BookDewarpTool) からも
// 既存の変換パイプライン (SuperPdfUtil) からも呼び出せる。

#nullable enable

using System;
using System.Collections.Generic;
using System.Linq;
using OpenCvSharp;

namespace SuperBookTools;

public class BookDewarpOptions
{
    /// <summary>解析用縮小画像の長辺の最大ピクセル数</summary>
    public int AnalysisMaxSize = 2000;

    /// <summary>テキスト行を近似する多項式の次数</summary>
    public int PolyDegree = 4;

    /// <summary>テキスト行として採用する最小の幅 (ページ幅に対する割合)</summary>
    public double MinLineWidthRatio = 0.2;

    /// <summary>テキスト行による補正を行うのに必要な最小の行数</summary>
    public int MinLines = 3;

    /// <summary>変位の上限 (ページ高さに対する割合)。これを超える変位は打ち切る</summary>
    public double MaxDisplacementRatio = 0.08;

    /// <summary>最大変位がこれ (ページ高さに対する割合) 未満なら、歪みなしとみなして何もしない</summary>
    public double MinEffectRatio = 0.003;

    /// <summary>ノド付近の横方向の縮みを補正する</summary>
    public bool CorrectHorizontal = true;

    /// <summary>横方向補正の強さ (1.0 = 縦変位をそのまま奥行きとみなす)</summary>
    public double HorizontalStrength = 1.0;

    /// <summary>テキスト行が少ない場合 (縦書き等) に本文の上端・下端の包絡線を使う</summary>
    public bool UseEnvelopeFallback = true;
}

public class BookDewarpResult
{
    public bool Applied;
    public string Message = "";
    public int NumLineCurves;
    public int NumEnvelopeCurves;
    public double MaxDisplacementPx;
    public int OutputWidth;
    public int OutputHeight;

    public override string ToString()
        => $"Applied={Applied}, Lines={NumLineCurves}, Envelopes={NumEnvelopeCurves}, MaxDisp={MaxDisplacementPx:F1}px, Out={OutputWidth}x{OutputHeight}, {Message}";
}

public static class BookDewarper
{
    // 1 本の曲線 (解析座標系)
    class Curve
    {
        public double[] Poly = Array.Empty<double>(); // u = (x - cx) / cx の多項式
        public double LineA, LineB;                   // 直線 y = A + B x (曲がっていない場合の位置)
        public double XMin, XMax;
        public double SlopeMin, SlopeMax;              // 両端での変位の傾き (外挿用)
        public double DispMin, DispMax;                // 両端での変位
        public bool IsEnvelope;
        public double Cx;

        public double U(double x) => (x - Cx) / Cx;
        public double Y(double x) => PolyEval(Poly, U(x));
        public double Target(double x) => LineA + LineB * x;

        public double Disp(double x, double maxDisp)
        {
            double d;
            if (x < XMin) d = DispMin + SlopeMin * (x - XMin);
            else if (x > XMax) d = DispMax + SlopeMax * (x - XMax);
            else d = Y(x) - Target(x);
            return Math.Clamp(d, -maxDisp, maxDisp);
        }

        public double DistanceOutside(double x) => x < XMin ? XMin - x : (x > XMax ? x - XMax : 0);
    }

    const int GridCell = 8; // 変位場のグリッド間隔 (解析座標)

    /// <summary>
    /// 画像 (Gray / BGR / BGRA) の綴じ目付近の湾曲を補正した新しい Mat を返す。
    /// 補正不要・補正不能と判断した場合は元画像の複製を返す (result.Applied == false)。
    /// debugImage を渡すと、検出した曲線 (赤) と補正後の直線 (緑) を描いた解析画像を書き込む。
    /// </summary>
    public static Mat Dewarp(Mat src, BookDewarpOptions? options, out BookDewarpResult result, Mat? debugImage = null)
    {
        options ??= new BookDewarpOptions();
        result = new BookDewarpResult { OutputWidth = src.Width, OutputHeight = src.Height };

        if (src.Empty() || src.Width < 64 || src.Height < 64)
        {
            result.Message = "Image too small";
            return src.Clone();
        }

        // ---- 1. 解析用画像 ----
        using var gray = new Mat();
        if (src.Channels() == 4) Cv2.CvtColor(src, gray, ColorConversionCodes.BGRA2GRAY);
        else if (src.Channels() == 3) Cv2.CvtColor(src, gray, ColorConversionCodes.BGR2GRAY);
        else src.ConvertTo(gray, MatType.CV_8UC1);

        double scale = Math.Min(1.0, (double)options.AnalysisMaxSize / Math.Max(src.Width, src.Height));
        using var small = new Mat();
        if (scale < 1.0) Cv2.Resize(gray, small, new Size(Math.Max(1, (int)Math.Round(src.Width * scale)), Math.Max(1, (int)Math.Round(src.Height * scale))), 0, 0, InterpolationFlags.Area);
        else gray.CopyTo(small);
        scale = (double)small.Width / src.Width;

        int W = small.Width, H = small.Height;

        using var charMask = BuildCharMask(small, out double charHeight, out int numChars);
        if (numChars < 30 || charHeight <= 0)
        {
            result.Message = "Not enough text";
            return src.Clone();
        }

        double maxDisp = options.MaxDisplacementRatio * H;

        // ---- 2-3. テキスト行の曲線 ----
        var curves = DetectLineCurves(charMask, charHeight, options);
        result.NumLineCurves = curves.Count;

        if (curves.Count < options.MinLines && options.UseEnvelopeFallback)
        {
            var env = DetectEnvelopeCurves(charMask, charHeight, options);
            result.NumEnvelopeCurves = env.Count;
            curves.AddRange(env);
        }

        if (curves.Count < 2 || (result.NumEnvelopeCurves == 0 && curves.Count < options.MinLines))
        {
            result.Message = "Not enough text lines";
            return src.Clone();
        }

        // ---- 4. 変位場 D(x, y) (解析座標、出力側 y で引く) ----
        int gw = (W + GridCell - 1) / GridCell + 1;
        int gh = (H + GridCell - 1) / GridCell + 1;
        using var dispGrid = BuildDisplacementGrid(curves, W, gw, gh, maxDisp);

        Cv2.MinMaxLoc(dispGrid, out double minD, out double maxD);
        double maxAbs = Math.Max(Math.Abs(minD), Math.Abs(maxD));
        result.MaxDisplacementPx = maxAbs / scale;

        if (debugImage != null) DrawDebug(small, curves, debugImage);

        if (maxAbs < options.MinEffectRatio * H)
        {
            result.Message = "No significant curvature";
            return src.Clone();
        }

        // ---- 5. 横方向 (弧長) ----
        double[] sOfX = BuildArcLength(dispGrid, curves, gw, gh, options); // 長さ gw、解析座標
        double totalS = sOfX[gw - 1] * (double)W / ((gw - 1) * GridCell);
        int outW = Math.Max(1, (int)Math.Round(totalS / scale));
        int outH = src.Height;

        // ---- 6. 出力座標グリッド上でのソース座標オフセット ----
        int ogw = (int)Math.Ceiling(outW * scale / GridCell) + 1;
        using var offX = new Mat(gh, ogw, MatType.CV_32FC1);
        using var offY = new Mat(gh, ogw, MatType.CV_32FC1);
        var ox = new float[gh * ogw];
        var oy = new float[gh * ogw];
        dispGrid.GetArray(out float[] dg);

        for (int i = 0; i < ogw; i++)
        {
            double s = i * GridCell;                    // 出力 x (解析座標) = 弧長
            double xa = InvertMonotone(sOfX, s);         // ソース x (解析座標)
            double gx = Math.Clamp(xa / GridCell, 0, gw - 1);
            int gx0 = Math.Min((int)gx, gw - 2);
            double fx = gx - gx0;

            for (int j = 0; j < gh; j++)
            {
                double d = dg[j * gw + gx0] * (1 - fx) + dg[j * gw + gx0 + 1] * fx;
                ox[j * ogw + i] = (float)((xa - s) / scale);
                oy[j * ogw + i] = (float)(d / scale);
            }
        }
        offX.SetArray(ox);
        offY.SetArray(oy);

        // グリッドの節点 k は出力のフル解像度で k * GridCell / scale の位置にある
        var fullGridSize = new Size((int)Math.Round((ogw - 1) * GridCell / scale) + 1, (int)Math.Round((gh - 1) * GridCell / scale) + 1);
        using var offXFull = new Mat();
        using var offYFull = new Mat();
        Cv2.Resize(offX, offXFull, fullGridSize, 0, 0, InterpolationFlags.Linear);
        Cv2.Resize(offY, offYFull, fullGridSize, 0, 0, InterpolationFlags.Linear);

        var roi = new Rect(0, 0, Math.Min(outW, fullGridSize.Width), Math.Min(outH, fullGridSize.Height));
        using var mapX = new Mat(outH, outW, MatType.CV_32FC1, Scalar.All(0));
        using var mapY = new Mat(outH, outW, MatType.CV_32FC1, Scalar.All(0));
        using (var a = offXFull[roi]) using (var b = mapX[roi]) a.CopyTo(b);
        using (var a = offYFull[roi]) using (var b = mapY[roi]) a.CopyTo(b);

        using var idX = IdentityRow(outW);
        using var idY = IdentityCol(outH);
        using var repX = new Mat();
        using var repY = new Mat();
        Cv2.Repeat(idX, outH, 1, repX);
        Cv2.Repeat(idY, 1, outW, repY);
        Cv2.Add(mapX, repX, mapX);
        Cv2.Add(mapY, repY, mapY);

        var dst = new Mat();
        Cv2.Remap(src, dst, mapX, mapY, InterpolationFlags.Cubic, BorderTypes.Replicate);

        result.Applied = true;
        result.OutputWidth = dst.Width;
        result.OutputHeight = dst.Height;
        result.Message = "OK";
        return dst;
    }

    // ------------------------------------------------------------------
    // 文字マスク
    // ------------------------------------------------------------------
    static Mat BuildCharMask(Mat small, out double charHeight, out int numChars)
    {
        int W = small.Width, H = small.Height;

        int block = Math.Max(15, (Math.Min(W, H) / 30) | 1);
        using var adaptive = new Mat();
        Cv2.AdaptiveThreshold(small, adaptive, 255, AdaptiveThresholdTypes.MeanC, ThresholdTypes.BinaryInv, block, 12);

        // 一様に暗いノドの影などは、大津法で「インク」とされても局所適応では拾わないので AND を取る
        using var otsu = new Mat();
        Cv2.Threshold(small, otsu, 0, 255, ThresholdTypes.BinaryInv | ThresholdTypes.Otsu);

        using var bin = new Mat();
        Cv2.BitwiseAnd(adaptive, otsu, bin);

        // 外周 (スキャンの枠など) を除去
        int bx = Math.Max(2, (int)(W * 0.015)), by = Math.Max(2, (int)(H * 0.015));
        Cv2.Rectangle(bin, new Rect(0, 0, W, by), Scalar.All(0), -1);
        Cv2.Rectangle(bin, new Rect(0, H - by, W, by), Scalar.All(0), -1);
        Cv2.Rectangle(bin, new Rect(0, 0, bx, H), Scalar.All(0), -1);
        Cv2.Rectangle(bin, new Rect(W - bx, 0, bx, H), Scalar.All(0), -1);

        using var labels = new Mat();
        using var stats = new Mat();
        using var centroids = new Mat();
        int n = Cv2.ConnectedComponentsWithStats(bin, labels, stats, centroids, PixelConnectivity.Connectivity8, MatType.CV_32S);

        var keep = new bool[n];
        var heights = new List<int>();
        int maxH = Math.Max(6, H / 15), maxW = Math.Max(6, W / 8);
        for (int i = 1; i < n; i++)
        {
            int w = stats.At<int>(i, (int)ConnectedComponentsTypes.Width);
            int h = stats.At<int>(i, (int)ConnectedComponentsTypes.Height);
            int area = stats.At<int>(i, (int)ConnectedComponentsTypes.Area);
            if (h < 2 || area < 4 || h > maxH || w > maxW) continue;
            // 細長すぎる成分 (罫線・影の縁) は文字ではない
            if (w > h * 12 || h > w * 12) continue;
            keep[i] = true;
            if (h >= 3) heights.Add(h);
        }

        numChars = heights.Count;
        charHeight = 0;
        if (heights.Count > 0)
        {
            heights.Sort();
            // 句読点などの小さい成分に引きずられないよう、上位側の中央値を使う
            charHeight = heights[(int)(heights.Count * 0.65)];
        }

        labels.GetArray(out int[] lab);
        var mask = new byte[W * H];
        for (int p = 0; p < lab.Length; p++) if (keep[lab[p]]) mask[p] = 255;

        var result = new Mat(H, W, MatType.CV_8UC1);
        result.SetArray(mask);
        return result;
    }

    // ------------------------------------------------------------------
    // テキスト行の検出と近似
    // ------------------------------------------------------------------
    static List<Curve> DetectLineCurves(Mat charMask, double charHeight, BookDewarpOptions options)
    {
        int W = charMask.Width, H = charMask.Height;
        var curves = new List<Curve>();

        int kw = Math.Max(3, (int)Math.Round(charHeight * 1.3));
        using var kernel = Cv2.GetStructuringElement(MorphShapes.Rect, new Size(kw, 1));
        using var smeared = new Mat();
        Cv2.MorphologyEx(charMask, smeared, MorphTypes.Close, kernel);

        using var labels = new Mat();
        using var stats = new Mat();
        using var centroids = new Mat();
        int n = Cv2.ConnectedComponentsWithStats(smeared, labels, stats, centroids, PixelConnectivity.Connectivity8, MatType.CV_32S);

        var candIndex = new int[n];
        var cands = new List<(int label, int x, int w)>();
        for (int i = 1; i < n; i++)
        {
            candIndex[i] = -1;
            int x = stats.At<int>(i, (int)ConnectedComponentsTypes.Left);
            int w = stats.At<int>(i, (int)ConnectedComponentsTypes.Width);
            int h = stats.At<int>(i, (int)ConnectedComponentsTypes.Height);
            if (w < options.MinLineWidthRatio * W) continue;
            if (w < h * 6) continue;
            // 複数行が繋がったものは除外 (ただし湾曲で背が高くなる分は許す)
            if (h > charHeight * 2.5 + w * 0.06) continue;
            candIndex[i] = cands.Count;
            cands.Add((i, x, w));
        }
        if (cands.Count == 0) return curves;

        var sumY = cands.Select(c => new double[c.w]).ToArray();
        var cnt = cands.Select(c => new int[c.w]).ToArray();

        labels.GetArray(out int[] lab);
        for (int y = 0; y < H; y++)
        {
            int row = y * W;
            for (int x = 0; x < W; x++)
            {
                int l = lab[row + x];
                if (l == 0) continue;
                int ci = candIndex[l];
                if (ci < 0) continue;
                int lx = x - cands[ci].x;
                sumY[ci][lx] += y;
                cnt[ci][lx]++;
            }
        }

        double cx = W / 2.0;
        for (int ci = 0; ci < cands.Count; ci++)
        {
            var xs = new List<double>();
            var ys = new List<double>();
            for (int lx = 0; lx < cands[ci].w; lx++)
            {
                if (cnt[ci][lx] == 0) continue;
                xs.Add(cands[ci].x + lx);
                ys.Add(sumY[ci][lx] / cnt[ci][lx]);
            }
            if (xs.Count < cands[ci].w * 0.6) continue;

            double span = (xs[^1] - xs[0]) / W;
            int degree = span < 0.35 ? 2 : options.PolyDegree;

            var curve = FitCurve(xs, ys, degree, cx, charHeight * 0.35, robust: false);
            if (curve != null) curves.Add(curve);
        }

        return curves;
    }

    // 縦書き等のための、本文ブロック上端・下端の包絡線
    static List<Curve> DetectEnvelopeCurves(Mat charMask, double charHeight, BookDewarpOptions options)
    {
        int W = charMask.Width, H = charMask.Height;
        var curves = new List<Curve>();

        // 字形による上端・下端のばらつきを抑えるため、隣の文字・行間を横につないでから包絡線を取る
        int kw = Math.Max(3, (int)Math.Round(charHeight * 2.5));
        using var kernel = Cv2.GetStructuringElement(MorphShapes.Rect, new Size(kw, 1));
        using var joined = new Mat();
        Cv2.MorphologyEx(charMask, joined, MorphTypes.Close, kernel);
        joined.GetArray(out byte[] m);

        // 列ごとの最上部・最下部の文字画素
        var topXs = new List<double>(); var topYs = new List<double>();
        var botXs = new List<double>(); var botYs = new List<double>();
        for (int x = 0; x < W; x++)
        {
            int top = -1, bot = -1;
            for (int y = 0; y < H; y++) if (m[y * W + x] != 0) { top = y; break; }
            if (top < 0) continue;
            for (int y = H - 1; y >= 0; y--) if (m[y * W + x] != 0) { bot = y; break; }
            topXs.Add(x); topYs.Add(top);
            botXs.Add(x); botYs.Add(bot);
        }
        if (topXs.Count < W * 0.3) return curves;

        double cx = W / 2.0;
        foreach (var (xs, ys) in new[] { (topXs, topYs), (botXs, botYs) })
        {
            // 柱やノンブルなど一部だけ飛び出したものは外れ値として除く
            var c = FitCurve(xs, ys, Math.Min(3, options.PolyDegree), cx, charHeight * 0.6, robust: true, outlierThreshold: charHeight * 1.5, minInlierRatio: 0.5);
            if (c != null)
            {
                c.IsEnvelope = true;
                if (c.XMax - c.XMin >= W * 0.4) curves.Add(c);
            }
        }
        return curves;
    }

    static Curve? FitCurve(List<double> xs, List<double> ys, int degree, double cx, double maxRms,
        bool robust, double outlierThreshold = 0, double minInlierRatio = 0)
    {
        var px = xs.ToList();
        var py = ys.ToList();
        double[]? poly = null;
        int iterations = robust ? 10 : 1;

        for (int it = 0; it < iterations; it++)
        {
            if (px.Count < degree + 5) return null;
            poly = PolyFit(px.Select(x => (x - cx) / cx).ToList(), py, degree);
            if (poly == null) return null;
            if (!robust || it == iterations - 1) break;

            // しきい値は残差の中央値から徐々に outlierThreshold まで絞る (外れ値に引っ張られた初回の当てはめ対策)
            var res = px.Select((x, i) => Math.Abs(PolyEval(poly, (x - cx) / cx) - py[i])).ToArray();
            double th = Math.Max(outlierThreshold, 2.0 * res.OrderBy(r => r).ElementAt(res.Length / 2));

            var nx = new List<double>(); var ny = new List<double>();
            for (int i = 0; i < px.Count; i++)
            {
                if (res[i] <= th) { nx.Add(px[i]); ny.Add(py[i]); }
            }
            if (nx.Count == px.Count && th <= outlierThreshold) break;
            px = nx; py = ny;
        }
        if (poly == null || px.Count < xs.Count * minInlierRatio) return null;

        double ss = 0;
        for (int i = 0; i < px.Count; i++) { double r = PolyEval(poly, (px[i] - cx) / cx) - py[i]; ss += r * r; }
        double rms = Math.Sqrt(ss / px.Count);
        if (rms > maxRms) return null;

        var curve = new Curve { Poly = poly, Cx = cx, XMin = px[0], XMax = px[^1] };

        // 「平らな部分」に頑健に直線を当てはめる (ノド側の曲がった部分を外れ値として除く)
        var sx = new List<double>(); var sy = new List<double>();
        int step = Math.Max(1, px.Count / 200);
        for (int i = 0; i < px.Count; i += step) { sx.Add(px[i]); sy.Add(curve.Y(px[i])); }
        var w = Enumerable.Repeat(1.0, sx.Count).ToArray();
        double a = 0, b = 0;
        for (int it = 0; it < 6; it++)
        {
            if (!WeightedLineFit(sx, sy, w, out a, out b)) return null;
            var res = sx.Select((x, i) => Math.Abs(sy[i] - (a + b * x))).ToArray();
            double med = res.OrderBy(r => r).ElementAt(res.Length / 2);
            double th = Math.Max(0.5, med * 1.5);
            for (int i = 0; i < w.Length; i++) w[i] = res[i] <= th ? 1 : 0;
            if (w.Sum() < sx.Count * 0.3) break;
        }
        curve.LineA = a;
        curve.LineB = b;

        // 端点での変位と傾き (範囲外への外挿用、端の 3% の割線)
        double edge = Math.Max(2, (curve.XMax - curve.XMin) * 0.03);
        curve.DispMin = curve.Y(curve.XMin) - curve.Target(curve.XMin);
        curve.DispMax = curve.Y(curve.XMax) - curve.Target(curve.XMax);
        curve.SlopeMin = (curve.DispMin - (curve.Y(curve.XMin + edge) - curve.Target(curve.XMin + edge))) / -edge;
        curve.SlopeMax = (curve.DispMax - (curve.Y(curve.XMax - edge) - curve.Target(curve.XMax - edge))) / edge;
        return curve;
    }

    // ------------------------------------------------------------------
    // 変位場
    // ------------------------------------------------------------------
    static Mat BuildDisplacementGrid(List<Curve> curves, int W, int gw, int gh, double maxDisp)
    {
        var grid = new float[gw * gh];
        double tol = GridCell * 2;
        var items = new List<(double t, double d)>();

        for (int i = 0; i < gw; i++)
        {
            double x = i * GridCell;

            // その x を覆う曲線を使う。どれも覆わなければ (ノドの余白など)、最も近い端の曲線を外挿する
            var use = curves.Where(c => c.DistanceOutside(x) <= tol).ToList();
            if (use.Count == 0)
            {
                double minDist = curves.Min(c => c.DistanceOutside(x));
                use = curves.Where(c => c.DistanceOutside(x) <= minDist + W * 0.05).ToList();
            }

            items.Clear();
            foreach (var c in use) items.Add((c.Target(x), c.Disp(x, maxDisp)));
            items.Sort((p, q) => p.t.CompareTo(q.t));

            int k = 0;
            for (int j = 0; j < gh; j++)
            {
                double y = j * GridCell;
                double d;
                if (y <= items[0].t) d = items[0].d;
                else if (y >= items[^1].t) d = items[^1].d;
                else
                {
                    while (k < items.Count - 2 && items[k + 1].t < y) k++;
                    var p = items[k]; var q = items[k + 1];
                    double f = q.t - p.t < 1e-6 ? 0.5 : (y - p.t) / (q.t - p.t);
                    d = p.d + (q.d - p.d) * f;
                }
                grid[j * gw + i] = (float)d;
            }
        }

        var mat = new Mat(gh, gw, MatType.CV_32FC1);
        mat.SetArray(grid);
        // 曲線の入れ替わりによる段差をならす
        Cv2.GaussianBlur(mat, mat, new Size(0, 0), 1.5, 1.5, BorderTypes.Replicate);
        return mat;
    }

    // 横方向: 本文範囲の平均変位を奥行きプロファイルとみなし、その弧長で x を伸ばす
    static double[] BuildArcLength(Mat dispGrid, List<Curve> curves, int gw, int gh, BookDewarpOptions options)
    {
        var s = new double[gw];
        for (int i = 0; i < gw; i++) s[i] = i * GridCell;
        if (!options.CorrectHorizontal || options.HorizontalStrength <= 0) return s;

        double tMin = curves.Min(c => Math.Min(c.Target(c.XMin), c.Target(c.XMax)));
        double tMax = curves.Max(c => Math.Max(c.Target(c.XMin), c.Target(c.XMax)));
        int j0 = Math.Clamp((int)(tMin / GridCell), 0, gh - 1);
        int j1 = Math.Clamp((int)Math.Ceiling(tMax / GridCell), j0, gh - 1);

        dispGrid.GetArray(out float[] dg);
        var depth = new double[gw];
        for (int i = 0; i < gw; i++)
        {
            double sum = 0;
            for (int j = j0; j <= j1; j++) sum += Math.Abs(dg[j * gw + i]);
            depth[i] = sum / (j1 - j0 + 1) * options.HorizontalStrength;
        }

        for (int i = 1; i < gw; i++)
        {
            double dz = depth[i] - depth[i - 1];
            s[i] = s[i - 1] + Math.Sqrt(GridCell * GridCell + dz * dz);
        }
        return s;
    }

    static double InvertMonotone(double[] s, double v)
    {
        // s[i] = 弧長 (単調増加)、節点 i の x = i * GridCell
        if (v <= s[0]) return v;
        int lo = 0, hi = s.Length - 1;
        if (v >= s[hi]) return hi * GridCell + (v - s[hi]);
        while (hi - lo > 1)
        {
            int mid = (lo + hi) / 2;
            if (s[mid] <= v) lo = mid; else hi = mid;
        }
        double f = (v - s[lo]) / (s[hi] - s[lo]);
        return (lo + f) * GridCell;
    }

    static Mat IdentityRow(int n)
    {
        var a = new float[n];
        for (int i = 0; i < n; i++) a[i] = i;
        var m = new Mat(1, n, MatType.CV_32FC1);
        m.SetArray(a);
        return m;
    }

    static Mat IdentityCol(int n)
    {
        var a = new float[n];
        for (int i = 0; i < n; i++) a[i] = i;
        var m = new Mat(n, 1, MatType.CV_32FC1);
        m.SetArray(a);
        return m;
    }

    static void DrawDebug(Mat small, List<Curve> curves, Mat debugImage)
    {
        Cv2.CvtColor(small, debugImage, ColorConversionCodes.GRAY2BGR);
        foreach (var c in curves)
        {
            var pts = new List<Point>();
            var tgt = new List<Point>();
            for (double x = c.XMin; x <= c.XMax; x += 4)
            {
                pts.Add(new Point(x, c.Y(x)));
                tgt.Add(new Point(x, c.Target(x)));
            }
            var color = c.IsEnvelope ? new Scalar(255, 0, 255) : new Scalar(0, 0, 255);
            Cv2.Polylines(debugImage, new[] { tgt }, false, new Scalar(0, 180, 0), 1, LineTypes.AntiAlias);
            Cv2.Polylines(debugImage, new[] { pts }, false, color, 1, LineTypes.AntiAlias);
        }
    }

    // ------------------------------------------------------------------
    // 数値計算
    // ------------------------------------------------------------------
    static double PolyEval(double[] c, double u)
    {
        double r = 0;
        for (int i = c.Length - 1; i >= 0; i--) r = r * u + c[i];
        return r;
    }

    static double[]? PolyFit(IReadOnlyList<double> us, IReadOnlyList<double> ys, int degree)
    {
        int n = degree + 1;
        var a = new double[n, n + 1];
        var p = new double[2 * n];
        for (int k = 0; k < us.Count; k++)
        {
            double v = 1;
            for (int i = 0; i < 2 * n; i++) { p[i] = v; v *= us[k]; }
            for (int i = 0; i < n; i++)
            {
                for (int j = 0; j < n; j++) a[i, j] += p[i + j];
                a[i, n] += p[i] * ys[k];
            }
        }
        return SolveLinear(a, n);
    }

    static double[]? SolveLinear(double[,] a, int n)
    {
        for (int col = 0; col < n; col++)
        {
            int piv = col;
            for (int r = col + 1; r < n; r++) if (Math.Abs(a[r, col]) > Math.Abs(a[piv, col])) piv = r;
            if (Math.Abs(a[piv, col]) < 1e-12) return null;
            if (piv != col) for (int c = 0; c <= n; c++) (a[col, c], a[piv, c]) = (a[piv, c], a[col, c]);
            for (int r = 0; r < n; r++)
            {
                if (r == col) continue;
                double f = a[r, col] / a[col, col];
                if (f == 0) continue;
                for (int c = col; c <= n; c++) a[r, c] -= f * a[col, c];
            }
        }
        var x = new double[n];
        for (int i = 0; i < n; i++) x[i] = a[i, n] / a[i, i];
        return x;
    }

    static bool WeightedLineFit(List<double> xs, List<double> ys, double[] w, out double a, out double b)
    {
        double sw = 0, sx = 0, sy = 0, sxx = 0, sxy = 0;
        for (int i = 0; i < xs.Count; i++)
        {
            if (w[i] <= 0) continue;
            sw += w[i]; sx += w[i] * xs[i]; sy += w[i] * ys[i];
            sxx += w[i] * xs[i] * xs[i]; sxy += w[i] * xs[i] * ys[i];
        }
        a = b = 0;
        double det = sw * sxx - sx * sx;
        if (sw < 2 || Math.Abs(det) < 1e-9) return false;
        b = (sw * sxy - sx * sy) / det;
        a = (sy - b * sx) / sw;
        return true;
    }
}
