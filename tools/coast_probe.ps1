# Touch-coast probe (Windows).
#
# Injects a *real* touch flick (InitializeTouchInjection + InjectTouchInput —
# the driver, message loop and WM_TOUCH are the ones a real finger hits), then
# captures the window immediately while the ride continues and reports the
# vertical shift between consecutive captures. What "coast" means, measured:
# motion continuing *after the finger lifted*, well past the drag's own travel,
# decaying rather than stopping dead.
#
# Field notes carried over from scroll_probe.ps1: PrintWindow with PW_
# RENDERFULLCONTENT (2), match on subsampled pixels (a row-luminance signature
# is periodic with the line height and mis-reports), a non-periodic body so a
# one-line shift can never match better than the truth, and a genuinely
# foreground window or the touch lands elsewhere.

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Coast {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x, y; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINTER_INFO {
    public uint pointerType, pointerId, frameId, pointerFlags;
    public IntPtr sourceDevice, hwndTarget;
    public POINT ptPixelLocation, ptHimetricLocation, ptPixelLocationRaw, ptHimetricLocationRaw;
    public uint dwTime, historyCount; public int InputData; public uint dwKeyStates;
    public ulong PerformanceCount; public int ButtonChangeType;
  }
  [StructLayout(LayoutKind.Sequential)] public struct POINTER_TOUCH_INFO {
    public POINTER_INFO pointerInfo; public uint touchFlags, touchMask;
    public RECT rcContact, rcContactRaw; public uint orientation, pressure;
  }
  [StructLayout(LayoutKind.Sequential)] public struct BMIH { public uint size; public int w, h; public ushort planes, bits; public uint comp, sizeImg; public int xppm, yppm; public uint clrUsed, clrImportant; }

  [DllImport("user32.dll", SetLastError=true)] public static extern bool InitializeTouchInjection(uint maxCount, uint dwMode);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool InjectTouchInput(uint count, POINTER_TOUCH_INFO[] contacts);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
  [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern int GetDIBits(IntPtr dc, IntPtr bmp, int start, int lines, byte[] bits, ref BMIH h, int usage);

  public const uint DOWN = 0x00010000, UPDATE = 0x00020000, UP = 0x00040000;
  public const uint INRANGE = 0x00000002, INCONTACT = 0x00000004;
  public static double DragStartMS, DragEndMS;

  static POINTER_TOUCH_INFO Make(int sx, int sy) {
    var c = new POINTER_TOUCH_INFO();
    c.pointerInfo.pointerType = 2; // PT_TOUCH
    c.pointerInfo.pointerId = 0;
    c.pointerInfo.ptPixelLocation = new POINT { x = sx, y = sy };
    c.touchMask = 0x7; // CONTACTAREA | ORIENTATION | PRESSURE
    c.rcContact = new RECT { left = sx - 2, top = sy - 2, right = sx + 2, bottom = sy + 2 };
    c.orientation = 90; c.pressure = 32000;
    return c;
  }

  /// A flick: contact, then `steps` moves of `stepY` px each at ~16 ms, then
  /// the finger comes up in motion (that is the state that should coast).
  public static string Flick(IntPtr hwnd, int cx, int cy, int steps, int stepY) {
    if (!InitializeTouchInjection(10, 1)) return "init failed " + Marshal.GetLastWin32Error();
    var p = new POINT { x = cx, y = cy };
    if (!ClientToScreen(hwnd, ref p)) return "ClientToScreen failed";
    int sx = p.x, sy = p.y;
    var t0 = System.Diagnostics.Stopwatch.StartNew();
    DragStartMS = t0.Elapsed.TotalMilliseconds;
    var c = Make(sx, sy);
    c.pointerInfo.pointerFlags = DOWN | INRANGE | INCONTACT;
    if (!InjectTouchInput(1, new[] { c })) return "DOWN failed " + Marshal.GetLastWin32Error();
    for (int i = 1; i <= steps; i++) {
      System.Threading.Thread.Sleep(16);
      var m = Make(sx, sy + stepY * i);
      m.pointerInfo.pointerFlags = UPDATE | INRANGE | INCONTACT;
      if (!InjectTouchInput(1, new[] { m })) return "MOVE " + i + " failed " + Marshal.GetLastWin32Error();
    }
    DragEndMS = t0.Elapsed.TotalMilliseconds;
    var u = Make(sx, sy + stepY * steps);
    u.pointerInfo.pointerFlags = UP;
    if (!InjectTouchInput(1, new[] { u })) return "UP failed " + Marshal.GetLastWin32Error();
    return "flick " + (stepY * steps) + " px over " + Math.Round(DragEndMS - DragStartMS) + " ms, lifted in motion";
  }

  public static int W, H, GW, GH;
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
    byte[] g = new byte[cols * GH];
    for (int y = top; y < bottom; y++) {
      int row = y * w * 4, o = (y - top) * cols;
      for (int c2 = 0; c2 < cols; c2++) {
        int i = row + (x0 + c2 * 3) * 4;
        g[o + c2] = (byte)((bits[i] + bits[i + 1] + bits[i + 2]) / 3);
      }
    }
    return g;
  }

  public static long LastCost;
  static long Diff(byte[] a, byte[] b, int gw, int gh, int s) {
    if (s < 0 || gh <= s) return long.MaxValue;
    long cost = 0; int n = (gh - s) * gw;
    for (int y = 0; y < gh - s; y++) {
      int ao = (y + s) * gw, bo = y * gw;
      for (int c2 = 0; c2 < gw; c2++) { int d = a[ao + c2] - b[bo + c2]; if (d < 0) d = -d; cost += d; }
    }
    return n > 0 ? cost / n : long.MaxValue;
  }
  public static int Shift(byte[] a, byte[] b, int gw, int gh, int max) {
    int best = 0; long bestCost = long.MaxValue;
    for (int s = 0; s <= max && s < gh; s++) {
      long cost = Diff(a, b, gw, gh, s);
      if (cost < bestCost) { bestCost = cost; best = s; }
    }
    LastCost = bestCost;
    return best;
  }
}
"@

$exe = Join-Path (Resolve-Path "$PSScriptRoot\..") "target\release\editor.exe"
if (-not (Test-Path $exe)) { throw "build it first: cargo build --release" }
$T = $env:TEMP

# The body has to be non-periodic (see the header note): otherwise a shift of
# exactly one line matches text-against-text and mis-reports the travel.
$words = @('alpha','bravo','charlie','delta','echo','foxtrot','golf','hotel',
    'india','juliet','kilo','lima','mike','november','oscar','papa')
$lines = @('# Coast probe')
for ($i = 1; $i -le 700; $i++) {
    $n = ($i * 13) % 9 + 3
    $w = @()
    for ($j = 0; $j -lt $n; $j++) { $w += $words[($i * 7 + $j * 11) % $words.Count] }
    $lines += "## $i " + ($w -join ' ')
}
$doc = Join-Path $T 'coast_probe.md'
$lines | Set-Content -Path $doc -Encoding utf8

$cfg = Join-Path $T ('editor_coast_' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $cfg | Out-Null
$env:APPDATA = $cfg

$p = Start-Process -FilePath $exe -ArgumentList $doc -PassThru
for ($i = 0; $i -lt 150; $i++) {
    Start-Sleep -Milliseconds 100
    $p.Refresh()
    if ($p.HasExited) { throw 'editor exited early' }
    if ($p.MainWindowHandle -ne 0) { break }
}
$h = $p.MainWindowHandle
$front = $false
for ($try = 0; $try -lt 30; $try++) {
    [void][Coast]::SetWindowPos($h, [IntPtr]::Zero, 80, 80, 1100, 800, 0x0040)
    [void][Coast]::SetForegroundWindow($h)
    Start-Sleep -Milliseconds 100
    if ([Coast]::GetForegroundWindow() -eq $h) { $front = $true; break }
}
if (-not $front) { Write-Host 'WARNING: could not bring the window to the foreground' }
Start-Sleep -Milliseconds 700

$r = New-Object Coast+RECT
[void][Coast]::GetWindowRect($h, [ref]$r)
$top = 48
$bottom = ($r.Bottom - $r.Top) - 48
Write-Host "window $($r.Right - $r.Left)x$($r.Bottom - $r.Top) matched rows $top..$bottom"
Write-Host ([Coast]::Flick($h, [int](($r.Left + $r.Right) / 2), 500, 10, -25))

# Capture immediately: the coast is what happens from here on, with no finger
# in contact. 1.6 s of samples.
$sw = [System.Diagnostics.Stopwatch]::StartNew()
[void][Coast]::Grid($h, $top, $bottom)
$base = [Coast]::Grid($h, $top, $bottom)
$gw = [Coast]::GW; $gh = [Coast]::GH
$row = @()
$prev = $base
$cum = 0.0
while ($sw.Elapsed.TotalMilliseconds -lt 1600) {
    $t = $sw.Elapsed.TotalMilliseconds
    $g = [Coast]::Grid($h, $top, $bottom)
    $s = [Coast]::Shift($prev, $g, $gw, $gh, 300)
    $cum += $s
    if ($s -ne 0) { $row += [pscustomobject]@{ t = [math]::Round($t, 0); px = $s; cum = [math]::Round($cum, 0); cost = [Coast]::LastCost } }
    $prev = $g
}
Write-Host ''
Write-Host '--- motion after the finger lifted (t_ms, px, cum, cost) ---'
$row | ForEach-Object { Write-Host ("  t=$($_.t) px=$($_.px) cum=$($_.cum) cost=$($_.cost)") }
$inMotion = @($row | Where-Object { $_.t -lt 400 })
Write-Host ('travel: ' + ($row | Select-Object -Last 1 -ExpandProperty cum) + ' px, motion lasted ' + ($row | Select-Object -Last 1 -ExpandProperty t) + ' ms past lift-off')

Stop-Process -Id $p.Id -Force
$p.WaitForExit()
Remove-Item -Recurse -Force $cfg
