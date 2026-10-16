param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/debug/delicium.exe'),
    [switch]$ValidateOnly
)

# Desktop-span acceptance: ordinary launch (no --overlay-size) must place the
# scene client exactly on the primary-monitor physical rectangle, keep the
# controls inside the monitor, and support dragging past the old bounded
# window edge. This harness never takes screenshots: it defines no Capture or
# EmptyArea helper at all, so a whole-desktop capture cannot happen here.
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$run = Join-Path $project ('.artifacts/windows-desktop-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff'))
[void][System.IO.Directory]::CreateDirectory($run)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$references = [System.IO.Directory]::GetFiles((Join-Path $PSHOME 'ref'), '*.dll') + @(
    [System.Windows.Forms.Form].Assembly.Location,
    [System.Windows.Forms.FormBorderStyle].Assembly.Location,
    [System.Reflection.Assembly]::Load('System.Windows.Forms.Primitives').Location,
    [System.Drawing.Bitmap].Assembly.Location,
    [System.Reflection.Assembly]::Load('System.Private.Windows.GdiPlus').Location,
    [System.Reflection.Assembly]::Load('System.Private.Windows.Core').Location
) | Select-Object -Unique
Add-Type -ReferencedAssemblies $references -TypeDefinition @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows.Forms;

public static class DesktopSpanNative {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct MouseInput { public int X, Y; public uint Data, Flags, Time; public UIntPtr Extra; }
    [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MouseInput Mouse; }
    [StructLayout(LayoutKind.Sequential)] public struct Input { public uint Type; public InputUnion Data; }
    public delegate bool Enumerate(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")] private static extern bool EnumWindows(Enumerate callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowText(IntPtr window, System.Text.StringBuilder text, int count);
    [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr window, out Rect rectangle);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr window, out Rect rectangle);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] private static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("user32.dll")] private static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] private static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] private static extern IntPtr GetWindowLongPtrW(IntPtr window, int index);
    [DllImport("gdi32.dll")] private static extern IntPtr CreateRectRgn(int left, int top, int right, int bottom);
    [DllImport("gdi32.dll")] private static extern bool DeleteObject(IntPtr region);
    [DllImport("gdi32.dll")] private static extern int GetRgnBox(IntPtr region, out Rect box);
    [DllImport("user32.dll")] private static extern int GetWindowRgn(IntPtr window, IntPtr region);

    public static IntPtr Find(uint process, string title) {
        IntPtr result = IntPtr.Zero;
        EnumWindows((window, unused) => {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner != process) return true;
            var text = new System.Text.StringBuilder(256);
            GetWindowText(window, text, text.Capacity);
            if (text.ToString() == title) result = window;
            return true;
        }, IntPtr.Zero);
        return result;
    }
    public static Rectangle Primary() { return Screen.PrimaryScreen.Bounds; }
    public static uint Owner(IntPtr window) { uint process; GetWindowThreadProcessId(window, out process); return process; }
    public static Rectangle Client(IntPtr window) {
        Rect rectangle; Point origin = new Point();
        if (!GetClientRect(window, out rectangle) || !ClientToScreen(window, ref origin)) throw new Exception("Cannot inspect owned client");
        return new Rectangle(origin.X, origin.Y, rectangle.Right, rectangle.Bottom);
    }
    public static Rectangle WindowRect(IntPtr window) {
        Rect rectangle;
        if (!GetWindowRect(window, out rectangle)) throw new Exception("Cannot inspect owned window");
        return Rectangle.FromLTRB(rectangle.Left, rectangle.Top, rectangle.Right, rectangle.Bottom);
    }
    public static bool Undecorated(IntPtr window) {
        Rect rectangle;
        if (!GetWindowRect(window, out rectangle)) throw new Exception("Cannot inspect owned window");
        var client = Client(window);
        return client.Left == rectangle.Left && client.Top == rectangle.Top && client.Right == rectangle.Right && client.Bottom == rectangle.Bottom;
    }
    public static void Raise(IntPtr window) {
        if (!SetWindowPos(window, new IntPtr(-1), 0, 0, 0, 0, 0x0013)) throw new Exception("Cannot raise owned window");
    }
    public static IntPtr Hit(int x, int y) { return GetAncestor(WindowFromPoint(new Point { X=x, Y=y }), 2); }
    public static void Mouse(uint flags) {
        var input = new Input { Type=0, Data=new InputUnion { Mouse=new MouseInput { Flags=flags } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send mouse input");
    }
    public static IntPtr Foreground() { return GetForegroundWindow(); }
    public static void Click(int x, int y, IntPtr expected) {
        if (Hit(x, y) != expected) throw new Exception("Click aborted: probe no longer belongs to expected owned window");
        if (!SetCursorPos(x, y)) throw new Exception("Cannot move test cursor");
        Thread.Sleep(40);
        if (Hit(x, y) != expected) throw new Exception("Click aborted after cursor settle");
        try { Mouse(2); Thread.Sleep(60); } finally { Mouse(4); }
    }
    public static bool PassesThrough(IntPtr window) { return (GetWindowLongPtrW(window, -20).ToInt64() & 0x20) != 0; }
    public static bool HasRegion(IntPtr window) {
        var region = CreateRectRgn(0, 0, 0, 0);
        if (region == IntPtr.Zero) throw new Exception("Cannot allocate region probe");
        try { return GetWindowRgn(window, region) != 0; } finally { DeleteObject(region); }
    }
    public static Rectangle RegionBox(IntPtr window) {
        var region = CreateRectRgn(0, 0, 0, 0);
        if (region == IntPtr.Zero) throw new Exception("Cannot allocate region probe");
        try {
            if (GetWindowRgn(window, region) == 0) throw new Exception("Scene has no region");
            Rect box;
            if (GetRgnBox(region, out box) == 0) throw new Exception("Cannot inspect region box");
            return Rectangle.FromLTRB(box.Left, box.Top, box.Right, box.Bottom);
        } finally { DeleteObject(region); }
    }
}

public sealed class DesktopSpanBackdrop : Form {
    private static readonly ManualResetEvent Ready = new ManualResetEvent(false);
    private static DesktopSpanBackdrop Instance;
    private static int clicks;
    private DesktopSpanBackdrop(Rectangle rectangle) {
        FormBorderStyle=FormBorderStyle.None; StartPosition=FormStartPosition.Manual;
        AutoScaleMode=AutoScaleMode.None; Bounds=rectangle; TopMost=true; ShowInTaskbar=false;
        BackColor=Color.FromArgb(36,58,78);
        MouseDown += (sender,args) => Interlocked.Increment(ref clicks);
        Shown += (sender,args) => Ready.Set();
    }
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams {
        get { var parameters=base.CreateParams; parameters.ExStyle |= 0x08000000; return parameters; }
    }
    public static IntPtr Open(Rectangle rectangle) {
        var thread=new Thread(() => {
            DesktopSpanNative.SetThreadDpiAwarenessContext(new IntPtr(-4));
            Instance=new DesktopSpanBackdrop(rectangle); Application.Run(Instance);
        });
        thread.IsBackground=true; thread.SetApartmentState(ApartmentState.STA); thread.Start();
        if (!Ready.WaitOne(5000)) throw new Exception("Backdrop did not become ready");
        return Instance.Handle;
    }
    public static int Clicks { get { return Volatile.Read(ref clicks); } }
    public static void AnchorBelow(IntPtr window) {
        Instance.Invoke(new Action(() => {
            Instance.TopMost=false; Instance.TopMost=true;
            if (!DesktopSpanNative.SetWindowPos(Instance.Handle,window,0,0,0,0,0x0013)) throw new Exception("Cannot anchor owned backdrop");
        }));
    }
    public static void Shut() {
        if (Instance!=null && !Instance.IsDisposed) Instance.BeginInvoke(new Action(() => Instance.Close()));
    }
}
'@

if ($ValidateOnly) {
    Write-Output '[OK] Desktop-span harness compiled without launching windows'
    return
}

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    Write-Output ('[OK] ' + $Message)
}
function Count-Log([string]$Pattern) {
    $lines = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') -ErrorAction SilentlyContinue)
    return @($lines | Select-String -Pattern $Pattern).Count
}
function Wait-Log([string]$Pattern, [int]$Count, [int]$TimeoutMs = 4000) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    while ((Count-Log $Pattern) -lt $Count -and [DateTime]::Now -lt $deadline) { Start-Sleep -Milliseconds 50 }
    Assert-Test ((Count-Log $Pattern) -ge $Count) ('Log observed: ' + $Pattern)
}

if ([DesktopSpanNative]::SetThreadDpiAwarenessContext([IntPtr]::new(-4)) -eq [IntPtr]::Zero) { throw 'Cannot enable physical-pixel DPI coordinates' }
$cursor = [DesktopSpanNative+Point]::new()
if (-not [DesktopSpanNative]::GetCursorPos([ref]$cursor)) { throw 'Cannot save the cursor position before testing' }
$process = $null; $scene = [IntPtr]::Zero; $controls = [IntPtr]::Zero; $backdrop = [IntPtr]::Zero; $mouseDown = $false
try {
    # Ordinary launch: no --overlay-size, so the scene must span the primary monitor.
    # --no-wander keeps the pinned positions this script's anchors assume.
    # The user's saved configuration must not leak into the test scene.
    $env:DELICIUM_NO_CONFIG = '1'
    $process = Start-Process -FilePath $Executable -ArgumentList '--no-wander', '--exit-after', '120' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError (Join-Path $run 'stderr.log')
    $deadline = [DateTime]::Now.AddSeconds(15)
    do {
        if ($process.HasExited) { throw ('Application exited before readiness; see ' + $run) }
        $scene = [DesktopSpanNative]::Find($process.Id, 'Delicium Scene')
        $controls = [DesktopSpanNative]::Find($process.Id, 'Delicium Controls')
        if ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero -and $scene -ne $controls) 'PID-scoped scene and control windows exist with independent handles'
    Assert-Test (([DesktopSpanNative]::Owner($scene) -eq $process.Id) -and ([DesktopSpanNative]::Owner($controls) -eq $process.Id)) 'Both window handles belong to the launched process'
    Wait-Log 'spawn instance=' 4 15000
    Wait-Log 'hit_region mode=pixel' 1
    Wait-Log 'scene_view' 1

    $monitor = [DesktopSpanNative]::Primary()
    $rectangle = [DesktopSpanNative]::Client($scene)
    Assert-Test ($rectangle.Left -eq $monitor.Left -and $rectangle.Top -eq $monitor.Top) 'Scene client origin equals the primary-monitor origin exactly'
    Assert-Test ($rectangle.Width -eq $monitor.Width -and $rectangle.Height -eq $monitor.Height) 'Scene client size equals the primary-monitor physical size exactly'
    Assert-Test ([DesktopSpanNative]::Undecorated($scene)) 'Client and window coordinates have the same origin and extent'
    Assert-Test ([DesktopSpanNative]::HasRegion($scene)) 'Default mode installs a native window region'
    Assert-Test (-not [DesktopSpanNative]::PassesThrough($scene)) 'Default mode is not whole-window pass-through'
    $box = [DesktopSpanNative]::RegionBox($scene)
    Assert-Test ($box.Left -ge 0 -and $box.Top -ge 0 -and $box.Right -le $rectangle.Width -and $box.Bottom -le $rectangle.Height -and $box.Width -gt 0 -and $box.Height -gt 0) 'Region union is nonempty and bounded by the client'

    # Anchor margins and the scene view derive from logical pixels at the
    # monitor DPI, so read the scene DPI once and scale physical expectations.
    $dpi = [DesktopSpanNative]::GetDpiForWindow($scene)
    if ($dpi -le 0) { throw 'Cannot read the scene DPI for anchor-margin and span expectations' }
    $scale = $dpi / 96.0
    $controlRect = [DesktopSpanNative]::Client($controls)
    Assert-Test ($controlRect.Left -ge $monitor.Left -and $controlRect.Top -ge $monitor.Top -and $controlRect.Right -le $monitor.Right -and $controlRect.Bottom -le $monitor.Bottom) 'Control window lies inside the primary monitor'
    $controlWindow = [DesktopSpanNative]::WindowRect($controls)
    $marginRight = $monitor.Right - $controlWindow.Right
    $marginTop = $controlWindow.Top - $monitor.Top
    Write-Output ('control margins right=' + $marginRight + ' top=' + $marginTop + ' scale=' + $scale)
    # The anchor keeps 50 logical pixels from the top and right edges; the
    # window frame can pull the measured window-rect margin inward, so bound
    # it at 70 logical pixels instead of a hardcoded physical number.
    $anchorLimit = [int](70.0 * $scale)
    Assert-Test ($marginRight -ge 0 -and $marginRight -le $anchorLimit) 'Control window keeps its top-right anchor margin on the right'
    Assert-Test ($marginTop -ge 0 -and $marginTop -le $anchorLimit) 'Control window keeps its top-right anchor margin on the top'
    $sampleX = $controlRect.Left + 18; $sampleY = $controlRect.Top + 12
    Assert-Test ([DesktopSpanNative]::Hit($sampleX, $sampleY) -eq $controls) 'Control sample point is owned by the control window'
    [DesktopSpanNative]::Click($sampleX, $sampleY, $controls)
    Start-Sleep -Milliseconds 150
    Assert-Test ([DesktopSpanNative]::Foreground() -eq $controls) 'Physical click can focus the control window in desktop mode'

    $viewLine = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') | Select-String 'scene_view') | Select-Object -Last 1
    $spanMatch = [regex]::Match([string]$viewLine, 'span=([0-9.]+)')
    $bottomMatch = [regex]::Match([string]$viewLine, 'bottom=(-?[0-9.]+)')
    Assert-Test ($spanMatch.Success -and $bottomMatch.Success) 'Startup log carries a parseable scene view'
    $span = [double]$spanMatch.Groups[1].Value
    $bottom = [double]$bottomMatch.Groups[1].Value
    Write-Output ('scene_view span=' + $span + ' bottom=' + $bottom)
    Assert-Test ($span -gt 0) 'Desktop scene view has a positive world span'
    Assert-Test ([Math]::Abs($bottom + 0.9) -lt 0.001) 'Desktop scene view keeps the legacy bottom at -0.9'
    # The span preserves the bounded scene's pixel-per-world-unit ratio at 640
    # logical pixels high; whether it grows past 4.5 depends on the monitor.
    $expectedSpan = 4.5 * $rectangle.Height / (640.0 * $scale)
    Assert-Test ([Math]::Abs($span - $expectedSpan) -lt 0.1) ('Desktop span matches the 640-logical-pixel formula: recorded=' + $span + ' expected=' + $expectedSpan)
    $viewTop = $bottom + $span
    function Screen-Point([double]$X, [double]$Y) {
        return @([int][Math]::Round((0.5 + $X / ($span * $rectangle.Width / $rectangle.Height)) * $rectangle.Width), [int][Math]::Round(($viewTop - $Y) / $span * $rectangle.Height))
    }

    $backdrop = [DesktopSpanBackdrop]::Open($monitor)
    Assert-Test (([DesktopSpanNative]::Owner($backdrop) -ne 0) -and ([DesktopSpanNative]::Owner($backdrop) -ne $process.Id)) 'Backdrop is owned by this test process, not the application'
    [DesktopSpanNative]::Raise($scene); [DesktopSpanNative]::Raise($controls)
    [DesktopSpanBackdrop]::AnchorBelow($scene)
    [DesktopSpanNative]::Raise($scene); [DesktopSpanNative]::Raise($controls)
    Start-Sleep -Milliseconds 500

    $emptyX = $rectangle.Right - 40; $emptyY = $rectangle.Bottom - 40
    Assert-Test ([DesktopSpanNative]::Hit($emptyX, $emptyY) -eq $backdrop) 'Empty far-corner probe belongs to the owned backdrop'
    $before = [DesktopSpanBackdrop]::Clicks; $dragBefore = Count-Log 'drag start instance='
    [DesktopSpanNative]::Click($emptyX, $emptyY, $backdrop)
    Start-Sleep -Milliseconds 150
    Assert-Test ([DesktopSpanBackdrop]::Clicks -eq ($before + 1)) 'Empty-area physical click reaches the backdrop in desktop mode'
    Assert-Test ((Count-Log 'drag start instance=') -eq $dragBefore) 'Empty-area click does not pick a character'

    $anchor = Screen-Point -2.4 0.15
    $anchorX = $rectangle.Left + $anchor[0]; $anchorY = $rectangle.Top + $anchor[1]
    Assert-Test ([DesktopSpanNative]::Hit($anchorX, $anchorY) -eq $scene) 'Character anchor receives input in desktop mode'

    # Drag past the edges of the former bounded window: 960x640 logical at
    # the 640-logical-pixel scale is 1440x960 physical, placed 30 logical
    # pixels right of the monitor origin in the old layout.
    $legacyRight = $rectangle.Left + [int](990 * $scale)
    # Aim 300 physical pixels past the old edge but stay comfortably inside
    # the monitor, so the release point remains over the owned scene.
    $targetX = [Math]::Min($legacyRight + 300, $rectangle.Right - 20)
    Assert-Test ($targetX -gt ($legacyRight + 10)) 'Drag target clears the former bounded-window right edge inside the monitor'

    $startBefore = Count-Log 'drag start instance='
    $releaseBefore = Count-Log 'drag release instance='
    $pixelBefore = Count-Log 'hit_region mode=pixel'
    $fullWindowBefore = Count-Log 'hit_region mode=full-window'
    [void][DesktopSpanNative]::SetCursorPos($anchorX, $anchorY)
    Start-Sleep -Milliseconds 150
    Assert-Test ([DesktopSpanNative]::Hit($anchorX, $anchorY) -eq $scene) 'Drag press is guarded against foreign windows'
    $mouseDown = $true; [DesktopSpanNative]::Mouse(2)
    Wait-Log 'drag start instance=' ($startBefore + 1)
    Wait-Log 'hit_region mode=full-window' ($fullWindowBefore + 1)
    Assert-Test (-not [DesktopSpanNative]::HasRegion($scene)) 'Drag temporarily clears the restrictive region'
    $steps = 12; $crossed = $false
    foreach ($step in 1..$steps) {
        $x = $anchorX + [int][Math]::Round(($targetX - $anchorX) * $step / [double]$steps)
        if ($x -gt $legacyRight) { $crossed = $true }
        Assert-Test ([DesktopSpanNative]::Hit($x, $anchorY) -eq $scene) 'Drag path stays in the owned scene'
        if (-not [DesktopSpanNative]::SetCursorPos($x, $anchorY)) { throw 'Cannot move drag cursor' }
        Start-Sleep -Milliseconds 45
    }
    Assert-Test $crossed 'Drag path samples beyond the former bounded-window right edge'
    [DesktopSpanNative]::Mouse(4); $mouseDown = $false
    Wait-Log 'drag release instance=' ($releaseBefore + 1)
    Wait-Log 'hit_region mode=pixel' ($pixelBefore + 1)
    $startLine = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') | Select-String 'drag start instance=') | Select-Object -Last 1
    $releaseLine = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') | Select-String 'drag release instance=') | Select-Object -Last 1
    $start = [regex]::Match([string]$startLine, 'drag start instance=(\d+) x=(-?[0-9.]+)')
    $release = [regex]::Match([string]$releaseLine, 'drag release instance=(\d+) x=(-?[0-9.]+)')
    Assert-Test ($start.Success -and $release.Success -and $start.Groups[1].Value -eq $release.Groups[1].Value) 'Press and release target the same character'
    $expectedWorld = ($targetX - $anchorX) * $span / $rectangle.Height
    $delta = [double]$release.Groups[2].Value - [double]$start.Groups[2].Value
    Assert-Test ([Math]::Abs($delta - $expectedWorld) -lt 0.25) ('Desktop drag moves the character by ' + [Math]::Round($delta, 3) + ' world units; expected about ' + [Math]::Round($expectedWorld, 3))
    Assert-Test ([DesktopSpanNative]::HasRegion($scene)) 'Release restores the native character region'
    $box = [DesktopSpanNative]::RegionBox($scene)
    Assert-Test ($box.Left -ge 0 -and $box.Top -ge 0 -and $box.Right -le $rectangle.Width -and $box.Bottom -le $rectangle.Height -and $box.Width -gt 0 -and $box.Height -gt 0) 'Restored region union is nonempty and bounded by the client'
    $before = [DesktopSpanBackdrop]::Clicks
    [DesktopSpanNative]::Click($emptyX, $emptyY, $backdrop)
    Start-Sleep -Milliseconds 150
    Assert-Test ([DesktopSpanBackdrop]::Clicks -eq ($before + 1)) 'Release restores empty-area pass-through'
    Assert-Test ((Count-Log 'hit_region mode=fallback') -eq 0) 'No hit-region fallback occurred during the desktop-span test'
    Write-Output ('WINDOWS_RESULT pass=true artifacts=' + $run)
} finally {
    try {
        if ($mouseDown) { [DesktopSpanNative]::Mouse(4) }
        [DesktopSpanBackdrop]::Shut()
        if ($process -and -not $process.HasExited) {
            if ($scene -ne [IntPtr]::Zero) { [void][DesktopSpanNative]::PostMessage($scene, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
            if (-not $process.WaitForExit(1500)) { Stop-Process -Id $process.Id -Force -Confirm:$false }
        }
    } finally {
        [void][DesktopSpanNative]::SetCursorPos($cursor.X, $cursor.Y)
    }
}
