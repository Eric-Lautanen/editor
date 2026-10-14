# Scroll-smoothness probe (Windows).
#
# Sends a *real* wheel notch (SendInput, MOUSEEVENTF_WHEEL) to a running
# editor and captures the window repeatedly while it moves, then reports the
# vertical shift between consecutive captures. That sequence is what "smooth"
# means: a view that jumps 48 px once has one step; a view that glides has a
# dozen small ones over ~150 ms.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/scroll_probe.ps1 `
#     -Exe target\release\editor.exe [-Notches 1] [-SampleMs 400] [-Repeat 3]
#
# Traps this cost me time:
#   * The app must be *foreground* and under the pointer: a wheel message goes
#     to the window under the cursor, so SetCursorPos comes first.
#   * PrintWindow's flags must be 2 (PW_RENDERFULLCONTENT). With 0 the window
#     renders empty, and a frame full of nothing has no shift to measure.
#   * Rows near the top and bottom are excluded from the match: the title bar
#     and the status bar do not scroll, and left in they pin the answer to
#     "shift 0" however far the document moved.
#   * Row signatures (a luminance sum per row over the middle columns) are
#     enough — pixel-exact comparison disagrees with itself over anti-aliased
#     glyphs and reports spurious one-pixel shifts.

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [int]$Notches = 1,
    [int]$SampleMs = 450,
    [int]$Repeat = 3,
    [switch]$DebugFrames,
    [switch]$Trace
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Scr {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x, y; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct BMIH { public uint size; public int w, h; public ushort planes, bits; public uint comp, sizeImg; public int xppm, yppm; public uint clrUsed, clrImportant; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public MOUSEINPUT mi; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, flags, time; public IntPtr extra; }

  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
  [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern int GetDIBits(IntPtr dc, IntPtr bmp, int start, int lines, byte[] bits, ref BMIH h, int usage);

  public static int W, H, GW, GH;
  /// The matched band of the window as it is right now (what the eye sees),
  /// subsampled to one byte per cell: rows are kept one for one (the shift is
  /// measured in pixels) and columns every third (only the pattern of ink
  /// matters).
  ///
  /// A per-row luminance total is *not* enough — it is periodic with the line
  /// height, so a shift of exactly one line matches text against text and wins
  /// over the true shift, which is rarely a whole number of lines.
  public static byte[] Grid(IntPtr hwnd, int top, int bottom) {
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
    int x0 = w / 5, x1 = (w * 4) / 5, cols = (x1 - x0) / 3;
    GW = cols; GH = bottom - top;
    byte[] g = new byte[cols * (bottom - top)];
    for (int y = top; y < bottom; y++) {
      int row = y * w * 4, o = (y - top) * cols;
      for (int c = 0; c < cols; c++) {
        int i = row + (x0 + c * 3) * 4;
        g[o + c] = (byte)((bits[i] + bits[i + 1] + bits[i + 2]) / 3);
      }
    }
    return g;
  }

  public static long LastCost;
  /// Mean cell difference at one fixed shift, for diagnosing a match that looks
  /// wrong: identical frames give 0 here.
  public static long Diff(byte[] a, byte[] b, int gw, int gh, int s) {
    if (s < 0 || gh <= s) return long.MaxValue;
    long cost = 0; int n = (gh - s) * gw;
    for (int y = 0; y < gh - s; y++) {
      int ao = (y + s) * gw, bo = y * gw;
      for (int c = 0; c < gw; c++) { int d = a[ao + c] - b[bo + c]; if (d < 0) d = -d; cost += d; }
    }
    return n > 0 ? cost / n : long.MaxValue;
  }

  /// How far down `b`'s content has moved relative to `a`, in pixels: the shift
  /// minimising the mean cell difference over the whole matched band.
  public static int Shift(byte[] a, byte[] b, int gw, int gh, int max) {
    int best = 0; long bestCost = long.MaxValue;
    for (int s = 0; s <= max && s < gh; s++) {
      long cost = Diff(a, b, gw, gh, s);
      if (cost < bestCost) { bestCost = cost; best = s; }
    }
    LastCost = bestCost;
    return best;
  }

  /// One wheel notch (`data` = -120 is "toward the user": the document scrolls
  /// down, which is the direction that moves content upward).
  public static void Wheel(int data) {
    var inp = new INPUT[1];
    inp[0].type = 0; // INPUT_MOUSE
    inp[0].mi.flags = 0x0800; // MOUSEEVENTF_WHEEL
    inp[0].mi.mouseData = unchecked((uint)data);
    SendInput(1, inp, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@

$exe = (Resolve-Path $Exe).Path
$T = $env:TEMP
$doc = Join-Path $T 'scroll_probe.md'

# The body has to be *non-periodic*, and that is not cosmetic: with the same
# paragraph repeated line for line, a shift of exactly one line matches text
# against text (only the paragraph number differs) and scores better than the
# true shift, which is not a whole number of lines. The probe then reports a
# one-line hop for a hundred-pixel glide. Varying both the words and the
# paragraph length makes every offset region look different from every other.
$words = @('alpha', 'bravo', 'charlie', 'delta', 'echo', 'foxtrot', 'golf',
    'hotel', 'india', 'juliet', 'kilo', 'lima', 'mike', 'november', 'oscar', 'papa')
$lines = @('# Scroll probe')
for ($i = 1; $i -le 400; $i++) {
    $n = ($i * 13) % 9 + 3
    $w = @()
    for ($j = 0; $j -lt $n; $j++) { $w += $words[($i * 7 + $j * 11) % $words.Count] }
    $lines += "## $i " + ($w -join ' ')
}
$lines | Set-Content -Path $doc -Encoding utf8

$cfg = Join-Path $T ('editor_scroll_' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $cfg | Out-Null
$env:APPDATA = $cfg

$traceLog = Join-Path $T 'scroll_trace.txt'
if ($Trace) {
    $env:EDITOR_TRACE_SCROLL = '1'
    if (Test-Path $traceLog) { Remove-Item $traceLog -Force }
    $p = Start-Process -FilePath $exe -ArgumentList $doc -PassThru -RedirectStandardError $traceLog
} else {
    $p = Start-Process -FilePath $exe -ArgumentList $doc -PassThru
}
for ($i = 0; $i -lt 150; $i++) {
    Start-Sleep -Milliseconds 100
    $p.Refresh()
    if ($p.HasExited) { throw "editor exited early" }
    if ($p.MainWindowHandle -ne 0) { break }
}
$h = $p.MainWindowHandle
# A wheel message goes to the window under the cursor, and nothing arrives at
# all if another window is in front — the probe then reports "no motion" for a
# scroll that never happened. Show the window, then wait until it really is
# foreground before measuring anything.
$front = $false
for ($try = 0; $try -lt 30; $try++) {
    [void][Scr]::SetWindowPos($h, [IntPtr]::Zero, 80, 80, 1100, 800, 0x0040) # SWP_SHOWWINDOW
    [void][Scr]::SetForegroundWindow($h)
    Start-Sleep -Milliseconds 100
    if ([Scr]::GetForegroundWindow() -eq $h) { $front = $true; break }
}
if (-not $front) { Write-Host 'WARNING: could not bring the window to the foreground' }
Start-Sleep -Milliseconds 700

$r = New-Object Scr+RECT
[void][Scr]::GetWindowRect($h, [ref]$r)
[void][Scr]::SetCursorPos([int](($r.Left + $r.Right) / 2), [int](($r.Top + $r.Bottom) / 2))
[void][Scr]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 300
$top = 48
$bottom = ($r.Bottom - $r.Top) - 48
Write-Host "window $($r.Right - $r.Left)x$($r.Bottom - $r.Top) matched rows $top..$bottom"
Write-Host "cells per frame: $([Scr]::GW) x $([Scr]::GH)"

$all = @()
for ($run = 1; $run -le $Repeat; $run++) {
    # If focus was lost between runs, the notches go nowhere.
    for ($try = 0; $try -lt 20; $try++) {
        [void][Scr]::SetForegroundWindow($h)
        Start-Sleep -Milliseconds 100
        if ([Scr]::GetForegroundWindow() -eq $h) { break }
    }
    [void][Scr]::Grid($h, $top, $bottom) # settle: let PrintWindow catch up
    Start-Sleep -Milliseconds 60
    $base = [Scr]::Grid($h, $top, $bottom)
    $gw = [Scr]::GW; $gh = [Scr]::GH
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    for ($n = 0; $n -lt $Notches; $n++) { [Scr]::Wheel(-120); Start-Sleep -Milliseconds 12 }
    $sent = $sw.Elapsed.TotalMilliseconds
    $sig = @()
    $t = @()
    while ($sw.Elapsed.TotalMilliseconds -lt $SampleMs) {
        $t += $sw.Elapsed.TotalMilliseconds
        $sig += , ([Scr]::Grid($h, $top, $bottom))
    }
    # Cumulative travel, sample by sample.
    $cum = 0.0
    $prev = $base
    $rows = @()
    for ($i = 0; $i -lt $sig.Count; $i++) {
        $s = [Scr]::Shift($prev, $sig[$i], $gw, $gh, 260)
        $cum += $s
        $rows += [pscustomobject]@{ t = [math]::Round($t[$i], 1); shift = $s; cum = $cum }
        $prev = $sig[$i]
    }
    if ($DebugFrames) {
        Write-Host "  frame diagnostics (t_ms, diff@shift0, best shift, cost):"
        $prev = $base
        for ($i = 0; $i -lt $sig.Count; $i++) {
            $s = [Scr]::Shift($prev, $sig[$i], $gw, $gh, 260)
            $c0 = [Scr]::Diff($prev, $sig[$i], $gw, $gh, 0)
            Write-Host ("    t=$([math]::Round($t[$i],1)) diff0=$c0 best=$s cost=$([Scr]::LastCost)")
            $prev = $sig[$i]
        }
    }
    $distinct = @($rows | Select-Object -ExpandProperty cum | Select-Object -Unique)
    $steps = @()
    $last = $null
    for ($i = 0; $i -lt $rows.Count; $i++) {
        if ($rows[$i].shift -ne 0) {
            $gap = if ($last -eq $null) { $null } else { [math]::Round($rows[$i].t - $last, 1) }
            $steps += [pscustomobject]@{ t = $rows[$i].t; moved = $rows[$i].shift; gap_ms = $gap }
            $last = $rows[$i].t
        }
    }
    $settled = if ($steps.Count -ge 2) { [math]::Round($steps[-1].t - $steps[0].t, 1) } else { 0 }
    $total = if ($distinct.Count -gt 0) { $distinct[-1] } else { 0 }
    $result = [pscustomobject]@{
        run          = $run
        notches      = $Notches
        notes_sent_t = [math]::Round($sent, 1)
        samples      = $sig.Count
        positions    = $distinct.Count
        total_px     = $total
        biggest_step = if ($steps.Count -gt 0) { ($steps | Measure-Object moved -Maximum).Maximum } else { 0 }
        median_gap   = if ($steps.Count -gt 1) { ($steps | Select-Object -Skip 1 | Measure-Object gap_ms -Average).Average } else { 0 }
        moved_frames = $steps.Count
        first_move_t = if ($steps.Count -gt 0) { $steps[0].t } else { $null }
        settled_ms   = $settled
    }
    $all += $result
    Write-Host ''
    Write-Host "--- run $run : $Notches notch(es) ---"
    Write-Host ("every frame with motion (t_ms, px): " + (($steps | ForEach-Object { "$($_.t)=$($_.moved)" }) -join ' '))
    Write-Host ("positions visited: $($distinct.Count)  total: $total px  biggest single step: $($result.biggest_step) px  motion over: $settled ms")
}

Write-Host ''
Write-Host 'summary (run, notches, samples, positions, total_px, biggest_step, moved_frames, first_move_t, settled_ms)'
$all | Format-Table -AutoSize | Out-String | Write-Host

Stop-Process -Id $p.Id -Force
$p.WaitForExit()
if ($Trace) {
    Write-Host ''
    Write-Host '--- app trace (scroll events, glide steps, clock) ---'
    Get-Content $traceLog | Select-Object -First 120 | Write-Host
}
Remove-Item -Recurse -Force $cfg
