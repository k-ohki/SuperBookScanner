using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using OpenCvSharp;
using SuperBookTools;

namespace BookDewarpTool;

public static class Program
{
    static readonly string[] ImageExtensions = { ".png", ".bmp", ".jpg", ".jpeg", ".tif", ".tiff" };

    public static int Main(string[] args)
    {
        var positional = args.Where(a => !a.StartsWith("--")).ToList();
        var flags = args.Where(a => a.StartsWith("--")).ToList();

        if (positional.Count != 2)
        {
            Console.WriteLine("Usage: BookDewarpTool <input file|dir> <output file|dir> [--debug] [--no-horizontal] [--strength=1.0]");
            Console.WriteLine("  Corrects the curvature of text lines near the book spine (gutter).");
            return 1;
        }

        var options = new BookDewarpOptions();
        bool debug = false;
        foreach (var f in flags)
        {
            if (f == "--debug") debug = true;
            else if (f == "--no-horizontal") options.CorrectHorizontal = false;
            else if (f.StartsWith("--strength=")) options.HorizontalStrength = double.Parse(f.Substring("--strength=".Length), System.Globalization.CultureInfo.InvariantCulture);
            else { Console.Error.WriteLine($"Unknown option: {f}"); return 1; }
        }

        string input = positional[0], output = positional[1];
        var jobs = new List<(string src, string dst)>();

        if (Directory.Exists(input))
        {
            Directory.CreateDirectory(output);
            foreach (var path in Directory.GetFiles(input).Where(p => ImageExtensions.Contains(Path.GetExtension(p).ToLowerInvariant())).OrderBy(p => p))
            {
                jobs.Add((path, Path.Combine(output, Path.GetFileName(path))));
            }
        }
        else if (File.Exists(input))
        {
            jobs.Add((input, output));
        }
        else
        {
            Console.Error.WriteLine($"Not found: {input}");
            return 1;
        }

        int numError = 0;
        Parallel.ForEach(jobs, new ParallelOptions { MaxDegreeOfParallelism = Environment.ProcessorCount }, job =>
        {
            try
            {
                using var src = Cv2.ImRead(job.src, ImreadModes.Unchanged);
                if (src.Empty()) throw new IOException("Failed to load image");

                using var debugImg = debug ? new Mat() : null;
                using var dst = BookDewarper.Dewarp(src, options, out var result, debugImg);

                Cv2.ImWrite(job.dst, dst);
                if (debugImg != null && !debugImg.Empty())
                {
                    Cv2.ImWrite(Path.ChangeExtension(job.dst, null) + ".dewarp_debug.png", debugImg);
                }
                Console.WriteLine($"{Path.GetFileName(job.src)}: {result}");
            }
            catch (Exception ex)
            {
                System.Threading.Interlocked.Increment(ref numError);
                Console.Error.WriteLine($"{job.src}: Error: {ex.Message}");
            }
        });

        return numError == 0 ? 0 : 2;
    }
}
