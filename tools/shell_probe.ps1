# Shell probe (Windows).
#
# Real-window runtime pass for the shell rounds (editor UX, recent files,
# theme, help/about): same PostMessage technique as find_probe.ps1 — the
# desktop has no foreground window while locked, so SendInput goes nowhere,
# but posted messages reach the same window proc the OS uses. Each step is
# verified by pixel-diffing PrintWindow captures; the Copy step is verified
# against the real clipboard.
#
#   1. click Edit      Preview -> Edit (the pane repaints)
#   2. F1              the help overlay opens over the pane
#   3. F2              help closes, about opens (the overlays are exclusive)
#   4. F2              about closes; the pane is back to its pre-overlay look
#   5. drag-select     a MouseDown/Move/Up sweep selects lines; the selection
#                      tints and the status bar grows the Copy button
#   6. click Copy      the selected text lands on the clipboard
#   7. right-click     the context menu opens at the pointer
#   8. Escape          the menu closes
#   9. thumb drag      dragging the scrollbar thumb scrolls the view
#  10. click Theme     dark <-> light flips the whole window
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\shell_probe.ps1 `
#     -Exe target\release\editor.exe [-DumpDir <dir>] [-DebugRows]

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [int]$SettleMs = 400,
    [string]$DumpDir = '',
    [switch]$DebugRows
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Sp {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct BMIH { public uint size; public int w, h; public ushort planes, bits; public uint comp, sizeImg; public int xppm, yppm; public uint clrUsed, clrImportant; }

  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
  [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern int GetDIBits(IntPtr dc, IntPtr bmp, int start, int lines, byte[] bits, ref BMIH h, int usage);

  public static int W, H;
  public static byte[] Capture(IntPtr hwnd) {
    RECT r; GetWindowRect(hwnd, out r);
    int w = r.Right - r.Left, h = r.Bottom - r.Top;
    IntPtr sdc = GetDC(IntPtr.Zero), mdc = CreateCompatibleDC(sdc);
    IntPtr bmp = CreateCompatibleBitmap(sdc, w, h), old = SelectObject(mdc, bmp);
    PrintWindow(hwnd, mdc, 2);
    var hdr = new BMIH(); hdr.size = (uint)Marshal.SizeOf(typeof(BMIH));
    hdr.w = w; hdr.h = -h; hdr.planes = 1; hdr.bits = 32; hdr.comp = 0;
    byte[] bits = new byte[w * 4 * h];
    GetDIBits(mdc, bmp, 0, h, bits, ref hdr, 0);
    SelectObject(mdc, old); DeleteObject(bmp); DeleteDC(mdc); ReleaseDC(IntPtr.Zero, sdc);
    W = w; H = h;
    return bits;
  }

  static int Lum(byte[] b, int i) { return (b[i] + b[i + 1] + b[i + 2]) / 3; }

  public static double Diff(byte[] a, byte[] b, int x0, int y0, int x1, int y1) {
    x0 = Math.Max(0, x0); y0 = Math.Max(0, y0);
    x1 = Math.Min(W, x1); y1 = Math.Min(H, y1);
    long cost = 0; long n = 0;
    for (int y = y0; y < y1; y++) {
      int row = y * W * 4;
      for (int x = x0; x < x1; x++) {
        int i = row + x * 4;
        cost += Math.Abs(a[i] - b[i]) + Math.Abs(a[i + 1] - b[i + 1]) + Math.Abs(a[i + 2] - b[i + 2]);
        n += 3;
      }
    }
    return n > 0 ? (double)cost / n : 0.0;
  }

  public static double MaxBandDiff(byte[] a, byte[] b, int x0, int y0, int x1, int y1) {
    double best = 0;
    for (int y = y0; y < y1; y += 50) {
      double d = Diff(a, b, x0, y, x1, Math.Min(y + 50, y1));
      if (d > best) best = d;
    }
    return best;
  }

  public static int[] Clusters(byte[] bits, int y0, int y1, int x0, int x1) {
    int[] hist = new int[16];
    for (int y = y0; y < y1; y++)
      for (int x = x0; x < x1; x++)
        hist[Lum(bits, (y * W + x) * 4) >> 4]++;
    int best = 0;
    for (int b = 1; b < 16; b++) if (hist[b] > hist[best]) best = b;
    int bg = best * 16 + 8;
    bool[] ink = new bool[x1 - x0];
    for (int x = x0; x < x1; x++) {
      int c = 0;
      for (int y = y0; y < y1; y++)
        if (Math.Abs(Lum(bits, (y * W + x) * 4) - bg) > 25) c++;
      ink[x - x0] = c > 0;
    }
    var centers = new List<int>();
    int run = -1, gap = 0;
    for (int x = 0; x < ink.Length; x++) {
      if (ink[x]) { if (run < 0) run = x; gap = 0; }
      else if (run >= 0) {
        gap++;
        if (gap > 2) {
          int w = x - gap - run;
          if (w >= 4) centers.Add(x0 + run + w / 2);
          run = -1; gap = 0;
        }
      }
    }
    if (run >= 0) {
      int w = ink.Length - run;
      if (w >= 4) centers.Add(x0 + run + w / 2);
    }
    return centers.ToArray();
  }

  public static int Bins(byte[] bits) {
    int[] hist = new int[16];
    for (int i = 0; i < bits.Length; i += 4) hist[Lum(bits, i) >> 4]++;
    int n = 0;
    for (int b = 0; b < 16; b++) if (hist[b] > bits.Length / 4 / 1000) n++;
    return n;
  }

  /// The vertical scrollbar thumb's centre y: in the right-edge rail band,
  /// the longest run of rows whose mean luminance strays from the band's
  /// modal row colour. Returns -1 when no thumb stands out.
  public static int ThumbCenter(byte[] bits) {
    int x0 = W - 8, x1 = W - 2, y0 = 60, y1 = H - 40;
    int[] rows = new int[y1 - y0];
    int[] hist = new int[16];
    for (int y = y0; y < y1; y++) {
      int sum = 0;
      for (int x = x0; x < x1; x++) sum += Lum(bits, (y * W + x) * 4);
      rows[y - y0] = sum / (x1 - x0);
      hist[rows[y - y0] >> 4]++;
    }
    int best = 0;
    for (int b = 1; b < 16; b++) if (hist[b] > hist[best]) best = b;
    int bg = best * 16 + 8;
    int runStart = -1, runLen = 0, bestStart = -1, bestLen = 0;
    for (int i = 0; i < rows.Length; i++) {
      if (Math.Abs(rows[i] - bg) > 20) {
        if (runStart < 0) runStart = i;
        runLen++;
        if (runLen > bestLen) { bestLen = runLen; bestStart = runStart; }
      } else { runStart = -1; runLen = 0; }
    }
    return bestLen >= 4 ? y0 + bestStart + bestLen / 2 : -1;
  }

  public static void SaveBmp(byte[] bits, string path) {
    int w = W, h = H, stride = w * 4;
    var fh = new byte[14]; var ih = new byte[40];
    fh[0] = (byte)'B'; fh[1] = (byte)'M';
    int total = 54 + stride * h;
    fh[2] = (byte)(total & 255); fh[3] = (byte)((total >> 8) & 255); fh[4] = (byte)((total >> 16) & 255); fh[5] = (byte)(total >> 24);
    fh[10] = 54;
    ih[4] = (byte)(w & 255); ih[5] = (byte)((w >> 8) & 255); ih[6] = (byte)((w >> 16) & 255); ih[7] = (byte)(w >> 24);
    int nh = -h;
    ih[8] = (byte)(nh & 255); ih[9] = (byte)((nh >> 8) & 255); ih[10] = (byte)((nh >> 16) & 255); ih[11] = (byte)(nh >> 24);
    ih[12] = 1; ih[14] = 32;
    using (var f = new System.IO.FileStream(path, System.IO.FileMode.Create)) {
      f.Write(fh, 0, 14); f.Write(ih, 0, 40);
      byte[] row = new byte[stride];
      for (int y = 0; y < h; y++) {
        System.Array.Copy(bits, y * stride, row, 0, stride);
        f.Write(row, 0, stride);
      }
    }
  }
}
"@
# --- part 2: flow ---
