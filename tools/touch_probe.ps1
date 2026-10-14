# touch_probe.ps1 — drive the real Editor window with a synthetic finger and
# capture what it did.
#
# Why this exists: the touch path cannot be reached from the headless suite or
# from `--render` (both stop at `App`), and it was wrong for a long time without
# anyone noticing — `TOUCHEVENTF_DOWN` and `TOUCHEVENTF_MOVE` were the wrong way
# round, so a finger drag dispatched no scroll at all and ended in a synthetic
# click where the finger came up (the caret jumped / text got selected). This
# harness is what caught it: build the debug binary, run this, then read the two
# captures back:
#
#   powershell -ExecutionPolicy Bypass -File tools/touch_probe.ps1
#   python - <<'PY'
#   import os
#   from PIL import Image, ImageChops
#   T = os.path.expandvars(r'%TEMP%')
#   b = Image.open(os.path.join(T,'touch_before.bmp')).convert('L')
#   a = Image.open(os.path.join(T,'touch_after.bmp')).convert('L')
#   print('changed:', sum(1 for p in ImageChops.difference(b,a).getdata() if p > 60))
#   PY
#
# A drag of 250 px up must leave the document scrolled by exactly 250 px: the
# before image, shifted 250 px, has to match the after image pixel for pixel.
# Requires a touch digitizer (the injection API fails without one, and a VM
# usually has none) — it injects *real* touch input, so the driver, the message
# loop and `WM_TOUCH` are all the ones the user hits.
#
# Notes for the next person, learned the hard way:
#   * `InitializeTouchInjection` needs `TOUCH_FEEDBACK_DEFAULT` (1).
#   * `pointerInfo.frameId` must stay 0, and `rcContactRaw` must be left alone:
#     setting either makes `InjectTouchInput` fail with ERROR_INVALID_PARAMETER
#     (87) *on the first call*, which reads like a bad struct layout.
#   * `POINTER_FLAG_UP` is 0x00040000 (0x80000 is WHEEL).
#   * The window must be on screen and under the injected point; touch is
#     delivered by hit test, not to the focused window alone.

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class TouchProbe {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int x, y; }
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINTER_INFO {
    public uint pointerType, pointerId, frameId, pointerFlags;
    public IntPtr sourceDevice, hwndTarget;
    public POINT ptPixelLocation, ptHimetricLocation, ptPixelLocationRaw, ptHimetricLocationRaw;
    public uint dwTime, historyCount;
    public int InputData;
    public uint dwKeyStates;
    public ulong PerformanceCount;
    public int ButtonChangeType;
  }
  [StructLayout(LayoutKind.Sequential)] public struct POINTER_TOUCH_INFO {
    public POINTER_INFO pointerInfo;
    public uint touchFlags, touchMask;
    public RECT rcContact, rcContactRaw;
    public uint orientation, pressure;
  }
  [DllImport("user32.dll", SetLastError=true)] public static extern bool InitializeTouchInjection(uint maxCount, uint dwMode);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool InjectTouchInput(uint count, POINTER_TOUCH_INFO[] contacts);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  const uint DOWN = 0x00010000, UPDATE = 0x00020000, UP = 0x00040000, INRANGE = 0x00000002, INCONTACT = 0x00000004;

  static POINTER_TOUCH_INFO Contact(int sx, int sy) {
    var c = new POINTER_TOUCH_INFO();
    c.pointerInfo.pointerType = 2;               // PT_TOUCH
    c.pointerInfo.ptPixelLocation = new POINT { x = sx, y = sy };
    c.touchMask = 0x7;                           // CONTACTAREA | ORIENTATION | PRESSURE
    c.rcContact = new RECT { left = sx - 2, top = sy - 2, right = sx + 2, bottom = sy + 2 };
    c.orientation = 90;
    c.pressure = 32000;
    return c;
  }

  /// A one-finger drag: down at client (cx, cy), `steps` moves of `stepY` px,
  /// then up. Returns what happened, or the failing call.
  public static string Drag(IntPtr hwnd, int cx, int cy, int steps, int stepY) {
    if (!InitializeTouchInjection(10, 1)) return "InitializeTouchInjection failed " + Marshal.GetLastWin32Error() + " (no touch digitizer?)";
    var p = new POINT { x = cx, y = cy };
    if (!ClientToScreen(hwnd, ref p)) return "ClientToScreen failed";
    int sx = p.x, sy = p.y;
    var d = Contact(sx, sy);
    d.pointerInfo.pointerFlags = DOWN | INRANGE | INCONTACT;
    if (!InjectTouchInput(1, new[] { d })) return "DOWN failed " + Marshal.GetLastWin32Error();
    for (int i = 1; i <= steps; i++) {
      System.Threading.Thread.Sleep(16);
      var m = Contact(sx, sy + stepY * i);
      m.pointerInfo.pointerFlags = UPDATE | INRANGE | INCONTACT;
      if (!InjectTouchInput(1, new[] { m })) return "MOVE " + i + " failed " + Marshal.GetLastWin32Error();
    }
    System.Threading.Thread.Sleep(16);
    var u = Contact(sx, sy + stepY * steps);
    u.pointerInfo.pointerFlags = UP;
    if (!InjectTouchInput(1, new[] { u })) return "UP failed " + Marshal.GetLastWin32Error();
    return "dragged from screen (" + sx + "," + sy + ") by " + (stepY * steps) + "px in " + steps + " steps";
  }
}
"@

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WindowCapture {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct BMIH { public uint size; public int w, h; public ushort planes, bits; public uint comp, sizeImg; public int xppm, yppm; public uint clrUsed, clrImportant; }
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
  [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern int GetDIBits(IntPtr dc, IntPtr bmp, int start, int lines, byte[] bits, ref BMIH h, int usage);
  /// The window as it looks right now, into a 32bpp top-down BMP.
  public static string Save(IntPtr hwnd, string path) {
    RECT r; GetWindowRect(hwnd, out r);
    int w = r.Right - r.Left, h = r.Bottom - r.Top;
    IntPtr sdc = GetDC(IntPtr.Zero), mdc = CreateCompatibleDC(sdc);
    IntPtr bmp = CreateCompatibleBitmap(sdc, w, h), old = SelectObject(mdc, bmp);
    PrintWindow(hwnd, mdc, 2);   // PW_RENDERFULLCONTENT
    var hdr = new BMIH(); hdr.size = (uint)Marshal.SizeOf(typeof(BMIH));
    hdr.w = w; hdr.h = -h; hdr.planes = 1; hdr.bits = 32; hdr.comp = 0;
    byte[] bits = new byte[w * 4 * h];
    GetDIBits(mdc, bmp, 0, h, bits, ref hdr, 0);
    SelectObject(mdc, old); DeleteObject(bmp); DeleteDC(mdc); ReleaseDC(IntPtr.Zero, sdc);
    using (var fs = new System.IO.FileStream(path, System.IO.FileMode.Create))
    using (var bw = new System.IO.BinaryWriter(fs)) {
      bw.Write((byte)'B'); bw.Write((byte)'M'); bw.Write(54 + bits.Length); bw.Write(0); bw.Write(54);
      bw.Write(40); bw.Write(w); bw.Write(-h); bw.Write((ushort)1); bw.Write((ushort)32);
      bw.Write(0); bw.Write(bits.Length); bw.Write(2835); bw.Write(2835); bw.Write(0); bw.Write(0);
      bw.Write(bits);
    }
    return w + "x" + h;
  }
}
"@

$exe = Join-Path $PSScriptRoot "..\target\debug\editor.exe"
if (-not (Test-Path $exe)) { throw "build it first: cargo build  ($exe)" }
$T = $env:TEMP
$doc = Join-Path $T "touchdoc.md"
$lines = @("# Touch test")
for ($i = 1; $i -le 300; $i++) { $lines += "Paragraph $i : the quick brown fox jumps over the lazy dog." }
$lines | Set-Content -Path $doc -Encoding utf8
# A throwaway config dir, so the developer's own session is not touched.
$cfg = Join-Path $T ("editor_touch_" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $cfg | Out-Null
$env:APPDATA = $cfg

$p = Start-Process -FilePath $exe -ArgumentList $doc -PassThru
for ($i = 0; $i -lt 150; $i++) {
  Start-Sleep -Milliseconds 100; $p.Refresh()
  if ($p.HasExited) { throw "the app exited during startup" }
  if ($p.MainWindowHandle -ne 0) { break }
}
$h = $p.MainWindowHandle
# Park it somewhere known, so the injected point is inside it by construction.
[void][WindowCapture]::SetWindowPos($h, [IntPtr]::Zero, 100, 100, 1100, 800, 0x0040)  # SWP_SHOWWINDOW
Start-Sleep -Milliseconds 1200
$r = New-Object WindowCapture+RECT
[void][WindowCapture]::GetClientRect($h, [ref]$r)
Write-Host "client $($r.Right)x$($r.Bottom)"
Write-Host ([WindowCapture]::Save($h, (Join-Path $T "touch_before.bmp")))
Write-Host ([TouchProbe]::Drag($h, [int]($r.Right / 2), 500, 10, -25))
Start-Sleep -Milliseconds 800
Write-Host ([WindowCapture]::Save($h, (Join-Path $T "touch_after.bmp")))

Stop-Process -Id $p.Id -Force; $p.WaitForExit()
Remove-Item -Recurse -Force $cfg
