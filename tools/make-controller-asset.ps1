# Builds assets/controller.bin from a controller line drawing (black lines on white).
#
# Output (little-endian): "XPAD", u16 width, u16 height, width*height line alpha bytes, then
# u8 region count and per region: u16 x, u16 y, u16 w, u16 h, w*h alpha bytes (0 or 255).
# Regions are the areas inside each control's outline, found by flood fill from seed points;
# the app lights them up under the lines. Order (must match src/padview.rs): LB, RB, LS, RS,
# View, Menu, Xbox, Y, X, A, B, D-pad up, down, left, right.
param(
  [Parameter(Mandatory)] [string]$Source,
  [string]$Out = (Join-Path $PSScriptRoot "..\assets\controller.bin")
)
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;

public static class PadAsset {
  // Crop of the source drawing that holds the controller.
  const int CX = 15, CY = 120, CW = 654, CH = 456;

  static byte[] lum; static int W, H;

  static bool Wall(int x, int y) { return lum[y * W + x] < 150; }

  // Flood fill (4-connected) from the seeds, limited to the box; then grow 2 px so the colour
  // reaches under the anti-aliased edge of the line.
  static bool[] Fill(int[][] seeds, int bx0, int by0, int bx1, int by1) {
    var m = new bool[W * H];
    var q = new Queue<int>();
    foreach (var s in seeds) {
      int sx = s[0], sy = s[1];
      if (Wall(sx, sy) || m[sy * W + sx]) continue;
      m[sy * W + sx] = true; q.Enqueue(sy * W + sx);
    }
    while (q.Count > 0) {
      int i = q.Dequeue(), x = i % W, y = i / W;
      int[,] nb = { { 1, 0 }, { -1, 0 }, { 0, 1 }, { 0, -1 } };
      for (int k = 0; k < 4; k++) {
        int nx = x + nb[k, 0], ny = y + nb[k, 1];
        if (nx < bx0 || ny < by0 || nx > bx1 || ny > by1) continue;
        int j = ny * W + nx;
        if (m[j] || Wall(nx, ny)) continue;
        m[j] = true; q.Enqueue(j);
      }
    }
    for (int pass = 0; pass < 2; pass++) {
      var g = (bool[])m.Clone();
      for (int y = by0; y <= by1; y++) for (int x = bx0; x <= bx1; x++) {
        if (m[y * W + x]) continue;
        if ((x > 0 && m[y * W + x - 1]) || (x < W - 1 && m[y * W + x + 1]) ||
            (y > 0 && m[(y - 1) * W + x]) || (y < H - 1 && m[(y + 1) * W + x])) g[y * W + x] = true;
      }
      m = g;
    }
    return m;
  }

  static int[][] Ring(int cx, int cy, int r) {
    var s = new List<int[]>();
    for (int a = 0; a < 360; a += 30) {
      double t = a * Math.PI / 180;
      s.Add(new[] { cx + (int)Math.Round(r * Math.Cos(t)), cy + (int)Math.Round(r * Math.Sin(t)) });
    }
    return s.ToArray();
  }

  static void Region(BinaryWriter w, bool[] m, List<string> log, string name) {
    int x0 = W, y0 = H, x1 = -1, y1 = -1, n = 0;
    for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) if (m[y * W + x]) {
      n++; if (x < x0) x0 = x; if (y < y0) y0 = y; if (x > x1) x1 = x; if (y > y1) y1 = y;
    }
    if (n == 0) throw new Exception("empty region " + name);
    int rw = x1 - x0 + 1, rh = y1 - y0 + 1;
    w.Write((ushort)(x0 - CX)); w.Write((ushort)(y0 - CY)); w.Write((ushort)rw); w.Write((ushort)rh);
    for (int y = y0; y <= y1; y++) for (int x = x0; x <= x1; x++) w.Write((byte)(m[y * W + x] ? 255 : 0));
    log.Add(string.Format("{0,-7} {1,5} px  at {2},{3}  {4}x{5}", name, n, x0, y0, rw, rh));
  }

  // The whole disk inside a round button's outline (letters and logos included, so a lit
  // button has no unlit holes): the circle that fits the flood-filled area.
  static bool[] Disk(bool[] m) {
    int x0 = W, y0 = H, x1 = -1, y1 = -1;
    for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) if (m[y * W + x]) {
      if (x < x0) x0 = x; if (y < y0) y0 = y; if (x > x1) x1 = x; if (y > y1) y1 = y;
    }
    double cx = (x0 + x1) / 2.0, cy = (y0 + y1) / 2.0, r = Math.Max(x1 - x0, y1 - y0) / 2.0 + 0.5;
    var d = new bool[W * H];
    for (int y = y0; y <= y1; y++) for (int x = x0; x <= x1; x++)
      d[y * W + x] = (x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r;
    return d;
  }

  // The source drawing has A on the right and B at the bottom; a real Xbox controller has
  // B on the right and A at the bottom. Swap what's inside the two circles (the letters).
  const int RightX = 560, RightY = 253, BottomX = 513, BottomY = 296, LetterR = 20;

  static void SwapLetters() {
    for (int dy = -LetterR; dy <= LetterR; dy++) for (int dx = -LetterR; dx <= LetterR; dx++) {
      if (dx * dx + dy * dy > LetterR * LetterR) continue;
      int a = (RightY + dy) * W + RightX + dx, b = (BottomY + dy) * W + BottomX + dx;
      byte t = lum[a]; lum[a] = lum[b]; lum[b] = t;
    }
  }

  // Sticks are drawn by the app (so they can move): cut them out of the line art.
  static readonly double[][] Sticks = { new[] { 173.5, 247.5 }, new[] { 426.0, 348.5 } };
  const double StickCut = 55;

  static bool InStick(int x, int y) {
    foreach (var s in Sticks) if ((x - s[0]) * (x - s[0]) + (y - s[1]) * (y - s[1]) <= StickCut * StickCut) return true;
    return false;
  }

  static bool[] Circle(int cx, int cy, int r, params int[][] extra) {
    var seeds = new List<int[]>(Ring(cx, cy, r - 7)); seeds.AddRange(extra);
    return Fill(seeds.ToArray(), cx - r - 5, cy - r - 5, cx + r + 5, cy + r + 5);
  }

  public static string Build(string src, string outPath) {
    var bmp = new Bitmap(src); W = bmp.Width; H = bmp.Height;
    var d = bmp.LockBits(new Rectangle(0, 0, W, H), ImageLockMode.ReadOnly, PixelFormat.Format24bppRgb);
    var raw = new byte[d.Stride * H]; Marshal.Copy(d.Scan0, raw, 0, raw.Length); bmp.UnlockBits(d); bmp.Dispose();
    lum = new byte[W * H];
    for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
      int i = y * d.Stride + x * 3; lum[y * W + x] = (byte)((raw[i] + raw[i + 1] + raw[i + 2]) / 3);
    }
    SwapLetters();
    var log = new List<string>();
    using (var w = new BinaryWriter(File.Create(outPath))) {
      w.Write(new[] { (byte)'X', (byte)'P', (byte)'A', (byte)'D' });
      w.Write((ushort)CW); w.Write((ushort)CH);
      for (int y = CY; y < CY + CH; y++) for (int x = CX; x < CX + CW; x++)
        w.Write(InStick(x, y) ? (byte)0 : (byte)Math.Min(255, Math.Max(0, (int)((255 - lum[y * W + x]) * 1.25))));
      w.Write((byte)15);
      Region(w, Fill(new[] { new[] { 185, 138 }, new[] { 150, 150 }, new[] { 220, 133 } }, 108, 116, 258, 172), log, "LB");
      Region(w, Fill(new[] { new[] { 500, 139 }, new[] { 535, 151 }, new[] { 465, 133 } }, 426, 116, 576, 172), log, "RB");
      // Stick areas (the app draws the sticks itself; kept so the file layout stays the same).
      Region(w, Circle(174, 247, 47, new[] { 174, 247 }), log, "LS");
      Region(w, Circle(426, 348, 48, new[] { 426, 348 }), log, "RS");
      Region(w, Disk(Circle(293, 248, 13, new[] { 293, 248 })), log, "View");
      Region(w, Disk(Circle(392, 248, 13, new[] { 392, 248 })), log, "Menu");
      Region(w, Disk(Circle(342, 182, 27)), log, "Xbox");
      Region(w, Disk(Circle(514, 205, 23)), log, "Y");
      Region(w, Disk(Circle(465, 248, 23)), log, "X");
      Region(w, Disk(Circle(BottomX, BottomY, 23)), log, "A");
      Region(w, Disk(Circle(RightX, RightY, 23)), log, "B");
      // D-pad: the cross, split into its four arms by the diagonals through the centre.
      int dx = 258, dy = 352;
      var cross = Fill(new[] { new[] { dx, dy - 22 }, new[] { dx, dy + 22 }, new[] { dx - 22, dy }, new[] { dx + 22, dy } }, dx - 50, dy - 50, dx + 50, dy + 50);
      string[] arms = { "D-up", "D-down", "D-left", "D-right" };
      for (int a = 0; a < 4; a++) {
        var arm = new bool[W * H];
        for (int y = dy - 50; y <= dy + 50; y++) for (int x = dx - 50; x <= dx + 50; x++) {
          if (!cross[y * W + x]) continue;
          int ox = x - dx, oy = y - dy;
          bool take = a == 0 ? -oy > Math.Abs(ox) && -oy > 6
                    : a == 1 ? oy > Math.Abs(ox) && oy > 6
                    : a == 2 ? -ox > Math.Abs(oy) && -ox > 6
                    : ox > Math.Abs(oy) && ox > 6;
          arm[y * W + x] = take;
        }
        Region(w, arm, log, arms[a]);
      }
    }
    return string.Join("\n", log);
  }
}
"@
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
[PadAsset]::Build((Resolve-Path $Source).Path, [IO.Path]::GetFullPath($Out))
"wrote $Out ($((Get-Item $Out).Length) bytes)"
