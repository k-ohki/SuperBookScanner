#pragma warning disable CA2235 // Mark all non-serializable fields

using System;
using System.Buffers;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using System.Collections.Generic;
using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Runtime.InteropServices;
using System.Runtime.CompilerServices;
using System.Diagnostics.CodeAnalysis;
using System.Runtime.Serialization;

using IPA.Cores.Basic;
using IPA.Cores.Helper.Basic;
using static IPA.Cores.Globals.Basic;

using IPA.Cores.Codes;
using IPA.Cores.Helper.Codes;
using static IPA.Cores.Globals.Codes;

using SuperBookTools;
using SuperBookTools.App;

namespace SuperBookTools.App
{
    public static class SuperBookExternalTools
    {
        public static readonly ImageMagickUtil ImageMagick = new ImageMagickUtil(new ImageMagickOptions(
            Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\ImageMagick-portable-Q16-HDRI-x64\magick.exe"),
            Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\ImageMagick-portable-Q16-HDRI-x64\mogrify.exe"),
            Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\exiftool-13.30_64\exiftool.exe"),
            Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\QPDF\bin\qpdf.exe"),
            Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\pdfcpu\pdfcpu.exe")
        ));

        public static readonly FfMpegUtil FfMpeg = new FfMpegUtil(new FfMpegUtilOptions(
            Path.Combine(Env.AppRootDir, @"_dummy.exe"),
            Path.Combine(Env.AppRootDir, @"_dummy.exe")));

        public static readonly PdfYomitokuLib YomiToku = new PdfYomitokuLib(Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\yomitoku"));

        public static readonly AiUtilBasicSettings Settings = new AiUtilBasicSettings
        {
            AiTest_RealEsrgan_BaseDir = Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\RealEsrgan\RealEsrgan_Repo"),
            AiTest_TesseractOCR_Data_Dir = Path.Combine(Env.AppRootDir, @"..\external_tools\external_tools\image_tools\TesseractOCR_Data"),
        };
        public static readonly AiTask Task = new AiTask(Settings, FfMpeg);

        public const string Post_OCR_Dir = "Post_OCR_Dir";
    }

    public static partial class Commands
    {
        [ConsoleCommand(
            "ConvertPdf command",
            "ConvertPdf [srcDir] [/dst:dstDir] [/ocr:yes|no]",
            "ConvertPdf command")]
        public static async Task<int> ConvertPdf(ConsoleService c, string cmdName, string str)
        {
            ConsoleParam[] args =
            {
                new ConsoleParam("[srcDir]", ConsoleService.Prompt, "Source directory path: ", ConsoleService.EvalNotEmpty, null),
                new ConsoleParam("dst", ConsoleService.Prompt, "Destination directory path: ", ConsoleService.EvalNotEmpty, null),
                new ConsoleParam("ocr", ConsoleService.Prompt, "Perform Japanese High-Quality OCR? (Y/N): ", null, null),
            };
            ConsoleParamValueList vl = c.ParseCommandList(cmdName, str, args);

            var (srcDir, dstDir) = await PrepareSrcDstDirAsync(vl.DefaultParam.StrValue, vl["dst"].StrValue);

            SuperPerformPdfOptions options = new SuperPerformPdfOptions {/* MaxPagesForDebug = 120, SaveDebugPng = true, SkipRealesrgan = true */ };

            var srcFiles = (await Lfs.EnumDirectoryAsync(srcDir, true)).Where(x => x.IsFile && x.Name.StartsWith("_") == false && x.Name._IsExtensionMatch(".pdf")).OrderBy(x => x.FullPath, StrCmpi)._Shuffle().ToList();

            var jobs = srcFiles.Select(src =>
            {
                string dstPath = PP.Combine(dstDir, PP.GetRelativeFileName(src.FullPath, srcDir));
                return new ConvertJob(src.FullPath, dstPath, () => SuperPdfUtil.PerformPdfAsync(src.FullPath, dstPath, options));
            }).ToList();

            await RunConvertJobsAsync(cmdName, jobs, dstDir, vl["ocr"].BoolValue);

            return 0;
        }

        [ConsoleCommand(
            "ConvertImages command",
            "ConvertImages [srcDir] [/dst:dstDir] [/ocr:yes|no]",
            "Convert book page images (jpg/png/tif/...) into PDF. Each directory that directly contains image files is treated as one book.")]
        public static async Task<int> ConvertImages(ConsoleService c, string cmdName, string str)
        {
            ConsoleParam[] args =
            {
                new ConsoleParam("[srcDir]", ConsoleService.Prompt, "Source directory path: ", ConsoleService.EvalNotEmpty, null),
                new ConsoleParam("dst", ConsoleService.Prompt, "Destination directory path: ", ConsoleService.EvalNotEmpty, null),
                new ConsoleParam("ocr", ConsoleService.Prompt, "Perform Japanese High-Quality OCR? (Y/N): ", null, null),
            };
            ConsoleParamValueList vl = c.ParseCommandList(cmdName, str, args);

            var (srcDir, dstDir) = await PrepareSrcDstDirAsync(vl.DefaultParam.StrValue, vl["dst"].StrValue);

            SuperPerformPdfOptions options = new SuperPerformPdfOptions();

            // srcDir 自身とそのサブディレクトリのうち、画像ファイルを直接含むものを 1 冊として扱う
            // ("_" で始まるディレクトリと、出力先ディレクトリ配下は除外)
            List<string> bookDirs = new() { srcDir };
            bookDirs.AddRange((await Lfs.EnumDirectoryAsync(srcDir, true))
                .Where(x => x.IsDirectory && x.IsCurrentOrParentDirectory == false)
                .Select(x => x.FullPath)
                .Where(x => PP.GetRelativeFileName(x, srcDir)._Split(StringSplitOptions.RemoveEmptyEntries, '/', '\\').Any(y => y.StartsWith("_")) == false)
                .Where(x => x._IsSamei(dstDir) == false && x.StartsWith(dstDir + PP.DirectorySeparator, StringComparison.OrdinalIgnoreCase) == false)
                .OrderBy(x => x, NaturalStrComparer.Instance));

            List<ConvertJob> jobs = new();

            foreach (string bookDir in bookDirs)
            {
                if ((await SuperPdfUtil.EnumImageFilesAsync(bookDir)).Count == 0) continue;

                // srcDir/foo/bar/*.jpg -> dstDir/foo/bar.pdf, srcDir/*.jpg -> dstDir/<srcDir の名前>.pdf
                string relativePath = bookDir._IsSamei(srcDir) ? PP.GetFileName(srcDir) : PP.GetRelativeFileName(bookDir, srcDir);
                string dstPath = PP.Combine(dstDir, PP.RemoveLastSeparatorChar(relativePath) + ".pdf");

                jobs.Add(new ConvertJob(bookDir, dstPath, () => SuperPdfUtil.PerformImageDirAsync(bookDir, dstPath, options)));
            }

            await RunConvertJobsAsync(cmdName, jobs, dstDir, vl["ocr"].BoolValue);

            return 0;
        }

        record ConvertJob(string SrcPath, string DstPath, Func<Task<bool>> RunAsync);

        static async Task<(string SrcDir, string DstDir)> PrepareSrcDstDirAsync(string srcDir, string dstDir)
        {
            srcDir = PP.RemoveLastSeparatorChar(await Lfs.NormalizePathAsync(srcDir, normalizeRelativePathIfSupported: true));
            dstDir = PP.RemoveLastSeparatorChar(await Lfs.NormalizePathAsync(dstDir, normalizeRelativePathIfSupported: true));

            $"- Source Dir: \"{srcDir}\""._Print();
            $"- Destination Dir: \"{dstDir}\""._Print();

            if (srcDir._IsSamei(dstDir))
            {
                throw new CoresException("srcDir must not be same to dstDir.");
            }

            await Lfs.CreateDirectoryAsync(dstDir);

            return (srcDir, dstDir);
        }

        static async Task RunConvertJobsAsync(string cmdName, List<ConvertJob> jobs, string dstDir, bool performOcr)
        {
            if (performOcr)
            {
                ""._Print();
                "***"._Print();
                $"The \"ocr\" option is enabled. This OCR feature uses \"YomiKaku\" AI engine published by kotaro.kinoshita-san. Plesae read the https://github.com/kotaro-kinoshita/yomitoku/blob/cba0a134e0d2ad3bfdce163231b3cb91de07928e/README.md license document."._Print();
                "***"._Print();
                ""._Print();
            }

            int numTotal = jobs.Count;
            int numOk = 0;
            int numError = 0;
            int numSkip = 0;

            $"Total {numTotal} Files"._Error();

            int currentNumber = 0;

            List<string> errorFilesList = new();

            foreach (var job in jobs)
            {
                currentNumber++;

                $"<< {currentNumber} / {numTotal} >> '{job.SrcPath}' Start"._Error();

                try
                {
                    if (await job.RunAsync() == false)
                    {
                        numSkip++;
                        $"<< {currentNumber} / {numTotal} >> '{job.SrcPath}' Skip"._Error();
                    }
                    else
                    {
                        numOk++;
                        $"<< {currentNumber} / {numTotal} >> '{job.SrcPath}' OK"._Error();
                    }
                }
                catch (Exception ex)
                {
                    Con.WriteLine($"<< {currentNumber} / {numTotal} >> Error: {job.SrcPath} -> {job.DstPath}");
                    ex._Error();
                    errorFilesList.Add(job.SrcPath);
                    numError++;
                }
            }

            if (performOcr)
            {
                Con.WriteLine("Performing Japanese OCR started ...");

                await SuperBookExternalTools.YomiToku.PerformOcrDirAsync(dstDir, PP.Combine(dstDir, SuperBookExternalTools.Post_OCR_Dir), SuperBookExternalTools.Post_OCR_Dir);

                Con.WriteLine("Performing Japanese OCR completed.");
            }

            if (errorFilesList.Count >= 1)
            {
                $"--- Error files ---"._Error();
                foreach (var errFile in errorFilesList)
                {
                    $"- {errFile}"._Error();
                }
            }

            $"\n\n<< {cmdName} Result >>\nnumTotal = {numTotal}, numSkip = {numSkip}, numOk = {numOk}, numError = {numError}\n\n"._Error();
        }
    }
}
