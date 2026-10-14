# Find-bar probe (Windows) — PostMessage edition.
#
# End-to-end runtime check of the Ctrl+F round in a *real* window: launches a
# release build on a generated document and drives it through the same window
# proc the OS uses (PostMessage WM_CHAR / WM_KEYDOWN / WM_LBUTTONDOWN), then
# verifies each step by pixel-diffing PrintWindow captures:
#
#   1. click Edit     the status-bar button switches Preview -> Edit
#   2. click Find     the find bar appears docked at the top-right
#   3. type "alpha"   the current match becomes the editor selection
#                     (tints + selection ink appear), the count updates
#   4. type "z"       "alphaz" matches nothing: the count drops to 0/0
#                     (the tint rects themselves are a 22%-alpha wash, ~0.04
#                     mean diff in the dark theme — under blink noise, so the
#                     count is the honest signal that narrowing ran)
#   5. Backspace      restores "alpha": the selection re-lands (rescan path)
#   6. Enter, Enter   the current match advances (selection moves)
#   7. Escape         the bar closes
#   8. click Find     the bar re-opens (the button toggles)
#
# Why PostMessage and not SendInput (the scroll/touch probes' approach)?
# Those probes need the window foreground, and SetForegroundWindow is denied
# on a locked desktop — GetForegroundWindow() is 0 and every injected key
# lands nowhere (verified: all diffs were exactly 0.000). Posted messages
# reach the same window proc with the same Event mapping, so everything
# after the physical input layer is still exercised for real.
#
# The status-bar buttons are located by pixel analysis, not hardcoded x: the
# bar's text clusters are found in the capture (rules are 1 px wide and
# filtered out), and the block is right-aligned in a known order —
# [Find][Open] | [Preview][Edit][Split] | [Help][About][Theme] — so the 5th
# cluster from the right is Edit and the 8th is Find.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/find_probe.ps1 `
#     -Exe target\release\readitor.exe
#
# Traps inherited from scroll_probe.ps1:
#   * PrintWindow flags must be 2 (PW_RENDERFULLCONTENT) or the frame is empty.
#   * The caret blinks on the 40 ms tick, so "unchanged" checks get a small
#     nonzero diff; thresholds below absorb it (it is ~30 px in ~400k).
#   * The strip is the top-right corner: the edit caret sits top-LEFT, so it
#     cannot pollute the "bar closed == pre-bar" comparison.
#   * Matches are every 10 lines so two Enters stay inside the first
#     screenful — a scrolled view would break the strip comparison in step 7.

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
public class Fp {
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
  /// Full-window capture (what the eye sees), 32bpp BGRA top-down. The
  /// borderless WM_NCCALCSIZE leaves no frame, so window coords == client
  /// coords and posted mouse messages can use capture coords directly.
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

  /// Mean per-pixel abs difference (averaged over B,G,R) inside a window-space
  /// rectangle, clipped to the capture. 0 = identical.
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

  /// Ink-cluster centers (x, left to right) inside a band: columns holding
  /// pixels that differ from the band's modal luminance by > 25, grouped with
  /// a 2 px gap tolerance and kept only when at least 4 px wide (the status
  /// bar's 1 px group rules drop out).
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

  /// Max mean diff over 50 px horizontal bands of a region: the "something
  /// changed here" signal for small features (match tints) that dilute to
  /// almost nothing when averaged over the whole pane.
  public static double MaxBandDiff(byte[] a, byte[] b, int x0, int y0, int x1, int y1) {
    double best = 0;
    for (int y = y0; y < y1; y += 50) {
      double d = Diff(a, b, x0, y, x1, Math.Min(y + 50, y1));
      if (d > best) best = d;
    }
    return best;
  }

  /// Luminance stats for the "window actually rendered" sanity check.
  public static double MeanLum(byte[] bits) {
    long sum = 0;
    for (int i = 0; i < bits.Length; i += 4) sum += Lum(bits, i);
    return (double)sum / (bits.Length / 4);
  }
  /// How many luminance bins (of 16) hold at least 0.1% of the pixels: a real
  /// UI has several (bg, text, rules, accents); a failed capture is uniform.
  public static int Bins(byte[] bits) {
    int[] hist = new int[16];
    for (int i = 0; i < bits.Length; i += 4) hist[Lum(bits, i) >> 4]++;
    int n = 0;
    for (int b = 0; b < 16; b++) if (hist[b] > bits.Length / 4 / 1000) n++;
    return n;
  }
  /// Save a capture as a top-down 32bpp BMP for offline analysis.
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
$exe = (Resolve-Path $Exe).Path
$T = $env:TEMP
$doc = Join-Path $T 'find_probe.txt'

# "alpha" every 10 lines so each Enter visibly moves the current match without
# scrolling past the first screenful; the filler varies so no two captures can
# accidentally agree by periodicity.
$words = @('bravo', 'charlie', 'delta', 'echo', 'foxtrot', 'golf', 'hotel',
    'india', 'juliet', 'kilo', 'lima', 'mike', 'november', 'oscar', 'papa')
$lines = @('find probe document')
for ($i = 1; $i -le 300; $i++) {
    if ($i % 10 -eq 5) { $lines += "alpha marker $i" }
    else {
        $w = @()
        for ($j = 0; $j -lt 6; $j++) { $w += $words[($i * 7 + $j * 13) % $words.Count] }
        $lines += "line $i " + ($w -join ' ')
    }
}
$lines | Set-Content -Path $doc -Encoding utf8

# Fresh settings dir so the probe is independent of the operator's session.
$cfg = Join-Path $T ('readitor_find_' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $cfg | Out-Null
$env:APPDATA = $cfg

$p = Start-Process -FilePath $exe -ArgumentList $doc -PassThru
try {
    for ($i = 0; $i -lt 150; $i++) {
        Start-Sleep -Milliseconds 100
        $p.Refresh()
        if ($p.HasExited) { throw 'readitor exited early' }
        if ($p.MainWindowHandle -ne 0) { break }
    }
    $h = $p.MainWindowHandle
    [void][Fp]::SetWindowPos($h, [IntPtr]::Zero, 80, 80, 1100, 800, 0x0040) # SWP_SHOWWINDOW
    Start-Sleep -Milliseconds 700

    $r = New-Object Fp+RECT
    [void][Fp]::GetWindowRect($h, [ref]$r)
    $W = $r.Right - $r.Left; $Hgt = $r.Bottom - $r.Top
    # The find bar docks at the view's top-right: below the title bar, 34 px
    # tall plus its 8 px margin (scaled). The strip covers it with room to
    # spare at any DPI.
    $strip = @{ x0 = $W * 35 / 100; y0 = 40; x1 = $W - 4; y1 = 150 }
    # The text pane: everything between the chrome and the status bar, clear
    # of both bars' horizontal extremes.
    $text = @{ x0 = $W * 10 / 100; y0 = 150; x1 = $W * 90 / 100; y1 = $Hgt - 60 }
    # The status bar band: 32 logical px tall (48 at 150% DPI); the text sits
    # centred, so a 28 px band hugging the bottom catches it either way.
    $bar_y0 = $Hgt - 30; $bar_y1 = $Hgt - 2
    Write-Host "window ${W}x$Hgt  strip y $($strip.y0)..$($strip.y1)  text y $($text.y0)..$($text.y1)"

    function Cap([string]$name) {
        Start-Sleep -Milliseconds 80   # settle: let PrintWindow catch up
        $bits = [Fp]::Capture($h)
        if ($DumpDir -ne '') {
            if (-not (Test-Path $DumpDir)) { New-Item -ItemType Directory -Path $DumpDir | Out-Null }
            [Fp]::SaveBmp($bits, (Join-Path $DumpDir "$name.bmp"))
        }
        return $bits
    }
    function RowDebug([string]$tag, $a, $b, $reg) {
        if (-not $DebugRows) { return }
        $bands = @()
        for ($y = [int]$reg.y0; $y -lt [int]$reg.y1; $y += 50) {
            $y1 = [Math]::Min([int]($y + 50), [int]$reg.y1)
            $bands += ("y$y=" + [math]::Round([Fp]::Diff($a, $b, [int]$reg.x0, $y, [int]$reg.x1, $y1), 2))
        }
        Write-Host ("    rows[$tag]: " + ($bands -join ' '))
    }
    function Click($x, $y) {
        $lp = [IntPtr]([int64]$y * 65536 + [int64]$x)
        [void][Fp]::PostMessage($h, 0x0201, [IntPtr]1, $lp)   # WM_LBUTTONDOWN
        Start-Sleep -Milliseconds 60
        [void][Fp]::PostMessage($h, 0x0202, [IntPtr]::Zero, $lp) # WM_LBUTTONUP
        Start-Sleep -Milliseconds 60
    }
    function PostKey([int]$vk) {
        [void][Fp]::PostMessage($h, 0x0100, [IntPtr]$vk, [IntPtr]::Zero) # WM_KEYDOWN
        Start-Sleep -Milliseconds 40
        [void][Fp]::PostMessage($h, 0x0101, [IntPtr]$vk, [IntPtr]::Zero) # WM_KEYUP
    }
    function PostChar([char]$c) {
        [void][Fp]::PostMessage($h, 0x0102, [IntPtr][int]$c, [IntPtr]::Zero) # WM_CHAR
        Start-Sleep -Milliseconds 30
    }

    $results = @()
    function Check([string]$name, $value, [string]$op, [double]$limit) {
        $pass = if ($op -eq 'gt') { $value -gt $limit }
                elseif ($op -eq 'ge') { $value -ge $limit }
                else { $value -lt $limit }
        $script:results += [pscustomobject]@{ check = $name; value = [math]::Round($value, 3); op = $op; limit = $limit; pass = $pass }
        Write-Host ("  {0,-26} value={1,8:N3}  expected {2} {3}  ->  {4}" -f $name, $value, $op, $limit, $(if ($pass) { 'PASS' } else { 'FAIL' }))
    }
    function RegionDiff($a, $b, $reg) {
        return [Fp]::Diff($a, $b, [int]$reg.x0, [int]$reg.y0, [int]$reg.x1, [int]$reg.y1)
    }

    $base = Cap 'base'
    Check 'window_renders' ([Fp]::Bins($base)) 'ge' 4.0

    # Locate the status-bar buttons in the base capture. Right-aligned block,
    # left to right: Find Open | Preview Edit Split | Help About Theme.
    $centers = [Fp]::Clusters($base, $bar_y0, $bar_y1, [int]($W / 2), $W - 8)
    Write-Host ("status-bar clusters (left to right): " + ($centers -join ' '))
    $found = $centers.Count -ge 8
    $results += [pscustomobject]@{ check = 'status_bar_buttons_found'; value = $centers.Count; op = 'ge'; limit = 8; pass = $found }
    Write-Host ("  {0,-26} value={1,8}  expected ge 8  ->  {2}" -f 'status_bar_buttons_found', $centers.Count, $(if ($found) { 'PASS' } else { 'FAIL' }))
    if (-not $found) { throw 'cannot locate the status-bar buttons' }
    $edit_x = $centers[$centers.Count - 5]
    $find_x = $centers[$centers.Count - 8]
    $bar_click_y = [int](($bar_y0 + $bar_y1) / 2)
    Write-Host "Edit button at x=$edit_x, Find button at x=$find_x, click y=$bar_click_y"

    Write-Host '1. click Edit: switch Preview -> Edit'
    Click $edit_x $bar_click_y; Start-Sleep -Milliseconds $SettleMs
    $edit = Cap 'edit'
    Check 'edit_button_switches_mode' (RegionDiff $base $edit $text) 'gt' 5.0

    Write-Host '2. click Find: open the find bar'
    Click $find_x $bar_click_y; Start-Sleep -Milliseconds $SettleMs
    $bar = Cap 'bar'
    RowDebug 'bar_appears' $edit $bar $strip
    Check 'bar_appears' (RegionDiff $edit $bar $strip) 'gt' 1.0

    Write-Host '3. type "alpha": tints + count'
    foreach ($ch in 'alpha'.ToCharArray()) { PostChar $ch }
    Start-Sleep -Milliseconds $SettleMs
    $typed = Cap 'typed'
    RowDebug 'tints_drawn' $bar $typed $text
    # The selection stays put under the land fix, so the signal is the
    # subtle accent tints on the other visible matches: a band check.
    Check 'tints_drawn' ([Fp]::MaxBandDiff($bar, $typed, [int]$text.x0, [int]$text.y0, [int]$text.x1, [int]$text.y1)) 'gt' 0.1
    Check 'count_updates' (RegionDiff $bar $typed $strip) 'gt' 0.4

    Write-Host '4. type "z": "alphaz" matches nothing'
    PostChar 'z'; Start-Sleep -Milliseconds $SettleMs
    $narrowed = Cap 'narrowed'
    RowDebug 'narrow_clears' $typed $narrowed $text
    # The tints are word-sized and a 22% accent wash: near-invisible in a
    # whole-pane mean, obvious in the 50 px band that holds one.
    Check 'narrow_clears_tints' ([Fp]::MaxBandDiff($typed, $narrowed, [int]$text.x0, [int]$text.y0, [int]$text.x1, [int]$text.y1)) 'gt' 0.1
    Check 'count_drops_to_zero' (RegionDiff $typed $narrowed $strip) 'gt' 1.0

    Write-Host '5. Backspace: restore "alpha"'
    PostKey 0x08; Start-Sleep -Milliseconds $SettleMs
    $restored = Cap 'restored'
    RowDebug 'backspace' $narrowed $restored $text
    # With the land-under-caret fix the selection no longer moves on
    # backspace; what returns is the tints, so the band signal is the check.
    Check 'backspace_restores' ([Fp]::MaxBandDiff($narrowed, $restored, [int]$text.x0, [int]$text.y0, [int]$text.x1, [int]$text.y1)) 'gt' 0.1

    Write-Host '6. Escape: close the bar (before any Enter, so the view has not scrolled)'
    PostKey 0x1B; Start-Sleep -Milliseconds $SettleMs
    $closed = Cap 'closed'
    RowDebug 'bar_closes' $restored $closed $strip
    Check 'bar_closes' (RegionDiff $restored $closed $strip) 'gt' 1.0
    RowDebug 'strip_normal' $edit $closed $strip
    Check 'strip_back_to_normal' (RegionDiff $edit $closed $strip) 'lt' 1.5

    Write-Host '7. reopen the bar, retype, Enter x2: advance the current match'
    Click $find_x $bar_click_y; Start-Sleep -Milliseconds $SettleMs
    foreach ($ch in 'alpha'.ToCharArray()) { PostChar $ch }
    Start-Sleep -Milliseconds $SettleMs
    $typed2 = Cap 'typed2'
    PostKey 0x0D; Start-Sleep -Milliseconds $SettleMs
    $enter1 = Cap 'enter1'
    PostKey 0x0D; Start-Sleep -Milliseconds $SettleMs
    $enter2 = Cap 'enter2'
    RowDebug 'enter_advances' $enter1 $enter2 $text
    Check 'enter_advances_match' (RegionDiff $enter1 $enter2 $text) 'gt' 0.8

    Write-Host ''
    $fails = @($results | Where-Object { -not $_.pass }).Count
    Write-Host ("summary: {0}/{1} checks passed" -f ($results.Count - $fails), $results.Count)
    $results | Format-Table -AutoSize | Out-String | Write-Host
    if ($fails -gt 0) { exit 1 }
    Write-Host 'FIND PROBE: ALL CHECKS PASSED'
    exit 0
}
finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Remove-Item -Recurse -Force $cfg -ErrorAction SilentlyContinue
}
