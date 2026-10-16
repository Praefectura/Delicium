param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/debug/delicium.exe'),
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$run = Join-Path $project ('.artifacts/windows-pixel-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff'))
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

public static class PixelHitNative {
    // Fail closed if the scene silently grew to the full primary monitor:
    // this capture harness must never turn into a whole-desktop screenshot.
    public static bool SpansPrimaryMonitor(Rectangle rectangle) {
        var primary = Screen.PrimaryScreen;
        if (primary == null) return false;
        var bounds = primary.Bounds;
        return rectangle.Left == bounds.Left && rectangle.Top == bounds.Top
            && rectangle.Width == bounds.Width && rectangle.Height == bounds.Height;
    }
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
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
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
    public static Rectangle Client(IntPtr window) {
        Rect rectangle; Point origin = new Point();
        if (!GetClientRect(window, out rectangle) || !ClientToScreen(window, ref origin)) throw new Exception("Cannot inspect owned client");
        return new Rectangle(origin.X, origin.Y, rectangle.Right, rectangle.Bottom);
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
    public static bool Background(Color color) { return Math.Abs(color.R-36)<=6 && Math.Abs(color.G-58)<=6 && Math.Abs(color.B-78)<=6; }
    public static void Capture(IntPtr scene, IntPtr controls, IntPtr backdrop, uint appPid, string path) {
        uint owner;
        GetWindowThreadProcessId(scene, out owner);
        if (owner != appPid) throw new Exception("Capture aborted: scene owner changed");
        GetWindowThreadProcessId(controls, out owner);
        if (owner != appPid) throw new Exception("Capture aborted: control owner changed");
        GetWindowThreadProcessId(backdrop, out owner);
        if (owner != (uint)System.Diagnostics.Process.GetCurrentProcess().Id) throw new Exception("Capture aborted: backdrop owner changed");
        var rectangle = Client(scene);
        if (SpansPrimaryMonitor(rectangle)) throw new Exception("Capture aborted: scene client equals the primary-monitor rectangle (desktop-span mode must not be captured)");
        for (int y=2; y<rectangle.Height; y=Math.Min(y+32, rectangle.Height-1)) {
            for (int x=2; x<rectangle.Width; x=Math.Min(x+32, rectangle.Width-1)) {
                var hit = Hit(rectangle.Left+x, rectangle.Top+y);
                if (hit != scene && hit != controls && hit != backdrop) throw new Exception("Capture aborted: foreign window covers scene grid");
                if (x == rectangle.Width-1) break;
            }
            if (y == rectangle.Height-1) break;
        }
        using (var bitmap = new Bitmap(rectangle.Width, rectangle.Height)) {
            using (var graphics = Graphics.FromImage(bitmap)) graphics.CopyFromScreen(rectangle.Location, System.Drawing.Point.Empty, rectangle.Size);
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
    public static void EmptyArea(string path, int x, int y, int radius) {
        using (var bitmap = new Bitmap(path)) {
            for (int dy=-radius; dy<=radius; dy++) for (int dx=-radius; dx<=radius; dx++) {
                if (!Background(bitmap.GetPixel(x+dx, y+dy))) throw new Exception("Empty probe overlaps rendered content");
            }
        }
    }
    public static Point Anchor(string path, int x, int y, int radius) {
        using (var bitmap = new Bitmap(path)) {
            int best=0; Point result = new Point();
            for (int cy=Math.Max(0,y-radius); cy<Math.Min(bitmap.Height,y+radius); cy+=2) {
                for (int cx=Math.Max(0,x-radius); cx<Math.Min(bitmap.Width,x+radius); cx+=2) {
                    var color = bitmap.GetPixel(cx,cy);
                    int score=Math.Max(Math.Abs(color.R-36),Math.Max(Math.Abs(color.G-58),Math.Abs(color.B-78)));
                    if (score>best) { best=score; result=new Point {X=cx,Y=cy}; }
                }
            }
            if (best<100) throw new Exception("No visible character anchor");
            return result;
        }
    }
    public static Point Gap(string path, Rectangle search, int radius) {
        using (var bitmap = new Bitmap(path)) {
            int best=0; Point result = new Point();
            for (int y=Math.Max(radius,search.Top); y<Math.Min(bitmap.Height-radius,search.Bottom); y+=2) {
                int left=-1;
                for (int x=Math.Max(radius,search.Left); x<Math.Min(bitmap.Width-radius,search.Right); x++) {
                    if (!Background(bitmap.GetPixel(x,y))) { left=x; continue; }
                    if (left<0) continue;
                    int right=x;
                    while (right<Math.Min(bitmap.Width-radius,search.Right) && Background(bitmap.GetPixel(right,y))) right++;
                    if (right>=Math.Min(bitmap.Width-radius,search.Right)) break;
                    int width=right-left-1, middle=(left+right)/2;
                    if (width>best && width>2*radius) {
                        bool clear=true;
                        for (int dy=-radius; dy<=radius && clear; dy++) for (int dx=-radius; dx<=radius; dx++) {
                            if (!Background(bitmap.GetPixel(middle+dx,y+dy))) { clear=false; break; }
                        }
                        if (clear) { best=width; result=new Point {X=middle,Y=y}; }
                    }
                    x=right-1;
                }
            }
            if (best==0) throw new Exception("No empty gap bounded by character pixels on both sides");
            return result;
        }
    }
    public static void VisibleCharacters(string path) {
        using (var bitmap = new Bitmap(path)) {
            var corner=bitmap.GetPixel(6,6);
            if (corner.R!=36 || corner.G!=58 || corner.B!=78) throw new Exception("Transparent corner did not reveal backdrop");
            for (int part=0; part<4; part++) {
                int count=0;
                for (int y=0; y<bitmap.Height; y+=3) for (int x=part*bitmap.Width/4; x<(part+1)*bitmap.Width/4; x+=3) {
                    if (!Background(bitmap.GetPixel(x,y))) count++;
                }
                if (count<200) throw new Exception("A character column has too few visible samples");
            }
        }
    }
}

public sealed class PixelHitBackdrop : Form {
    private static readonly ManualResetEvent Ready = new ManualResetEvent(false);
    private static PixelHitBackdrop Instance;
    private static int clicks;
    private PixelHitBackdrop(Rectangle rectangle) {
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
            PixelHitNative.SetThreadDpiAwarenessContext(new IntPtr(-4));
            Instance=new PixelHitBackdrop(rectangle); Application.Run(Instance);
        });
        thread.IsBackground=true; thread.SetApartmentState(ApartmentState.STA); thread.Start();
        if (!Ready.WaitOne(5000)) throw new Exception("Backdrop did not become ready");
        return Instance.Handle;
    }
    public static int Clicks { get { return Volatile.Read(ref clicks); } }
    public static void AnchorBelow(IntPtr window) {
        Instance.Invoke(new Action(() => {
            Instance.TopMost=false; Instance.TopMost=true;
            if (!PixelHitNative.SetWindowPos(Instance.Handle,window,0,0,0,0,0x0013)) throw new Exception("Cannot anchor owned backdrop");
        }));
    }
    public static void Shut() {
        if (Instance!=null && !Instance.IsDisposed) Instance.BeginInvoke(new Action(() => Instance.Close()));
    }
}
'@

if ($ValidateOnly) {
    Write-Output '[OK] Pixel-hit harness compiled without launching windows'
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
function Toggle-Regions {
    $rect = [PixelHitNative]::Client($controls)
    $scale = $rect.Width / 380.0
    $hadRegion = [PixelHitNative]::HasRegion($scene)
    [PixelHitNative]::Click(($rect.Left + 18), ($rect.Top + 12), $controls)
    [PixelHitNative]::Click(($rect.Left + [int](12 * $scale)), ($rect.Top + [int](67 * $scale)), $controls)
    $deadline = [DateTime]::Now.AddSeconds(3)
    while ([PixelHitNative]::HasRegion($scene) -eq $hadRegion -and [DateTime]::Now -lt $deadline) { Start-Sleep -Milliseconds 50 }
    Assert-Test ([PixelHitNative]::HasRegion($scene) -ne $hadRegion) 'Native hit-region state follows the checkbox'
    Assert-Test (-not [PixelHitNative]::PassesThrough($scene)) 'Mode switch does not enter fallback'
}
function Screen-Point([double]$X, [double]$Y) {
    return @([int][Math]::Round((0.5 + $X / (4.5 * $rectangle.Width / $rectangle.Height)) * $rectangle.Width), [int][Math]::Round((3.6 - $Y) / 4.5 * $rectangle.Height))
}
function Capture-Scene([string]$Name) {
    $path = Join-Path $run $Name
    [PixelHitNative]::Capture($scene, $controls, $backdrop, $process.Id, $path)
    return $path
}

if ([PixelHitNative]::SetThreadDpiAwarenessContext([IntPtr]::new(-4)) -eq [IntPtr]::Zero) { throw 'Cannot enable physical-pixel DPI coordinates' }
$cursor = [PixelHitNative+Point]::new()
if (-not [PixelHitNative]::GetCursorPos([ref]$cursor)) { throw 'Cannot save the cursor position before testing' }
$process = $null; $scene = [IntPtr]::Zero; $backdrop = [IntPtr]::Zero; $mouseDown = $false
try {
    # The user's saved configuration must not leak into the test scene.
    $env:DELICIUM_NO_CONFIG = '1'
    $process = Start-Process -FilePath $Executable -ArgumentList '--overlay-size', '960x640', '--no-wander', '--exit-after', '120' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError (Join-Path $run 'stderr.log')
    $deadline = [DateTime]::Now.AddSeconds(15)
    do {
        if ($process.HasExited) { throw ('Application exited before readiness; see ' + $run) }
        $scene = [PixelHitNative]::Find($process.Id, 'Delicium Scene')
        $controls = [PixelHitNative]::Find($process.Id, 'Delicium Controls')
        if ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) 'Both PID-scoped application windows exist'
    Wait-Log 'spawn instance=' 4 15000
    Wait-Log 'hit_region mode=pixel' 1
    Assert-Test ([PixelHitNative]::Undecorated($scene)) 'Client and window coordinates have the same origin and extent'
    Assert-Test ([PixelHitNative]::HasRegion($scene)) 'Default mode installs a native window region'
    Assert-Test (-not [PixelHitNative]::PassesThrough($scene)) 'Default mode is not whole-window pass-through'
    $rectangle = [PixelHitNative]::Client($scene)
    $backdrop = [PixelHitBackdrop]::Open($rectangle)
    [PixelHitNative]::Raise($scene); [PixelHitNative]::Raise($controls)
    [PixelHitBackdrop]::AnchorBelow($scene)
    [PixelHitNative]::Raise($scene); [PixelHitNative]::Raise($controls)
    Start-Sleep -Milliseconds 500
    $first = Capture-Scene 'scene-first.png'
    [PixelHitNative]::VisibleCharacters($first)
    $origin = Screen-Point -2.4 0.15
    $anchor = [PixelHitNative]::Anchor($first, $origin[0], $origin[1], [int]($rectangle.Height / 12))
    $anchorX = $rectangle.Left + $anchor.X; $anchorY = $rectangle.Top + $anchor.Y
    Assert-Test ([PixelHitNative]::Hit($anchorX, $anchorY) -eq $scene) 'Visible character anchor receives input in default mode'
    $emptyX = $rectangle.Right - 40; $emptyY = $rectangle.Bottom - 40
    [PixelHitNative]::EmptyArea($first, ($rectangle.Width - 40), ($rectangle.Height - 40), 10)
    $before = [PixelHitBackdrop]::Clicks; $dragBefore = Count-Log 'drag start instance='
    [PixelHitNative]::Click($emptyX, $emptyY, $backdrop)
    Start-Sleep -Milliseconds 150
    Assert-Test ([PixelHitBackdrop]::Clicks -eq ($before + 1)) 'Empty-area physical click reaches the backdrop'
    Assert-Test ((Count-Log 'drag start instance=') -eq $dragBefore) 'Empty-area click does not pick a character'

    # These probes are chosen from color data alone, not from the region being
    # tested: a rectangle/whole-instance convex hull must fail the assertions.
    $earTop = Screen-Point -3.1 1.08; $earBottom = Screen-Point -1.7 0.82
    $earSearch = [System.Drawing.Rectangle]::FromLTRB($earTop[0], $earTop[1], $earBottom[0], $earBottom[1])
    $earGap = [PixelHitNative]::Gap($first, $earSearch, 10)
    $before = [PixelHitBackdrop]::Clicks
    [PixelHitNative]::Click(($rectangle.Left + $earGap.X), ($rectangle.Top + $earGap.Y), $backdrop)
    Start-Sleep -Milliseconds 150
    Assert-Test ([PixelHitBackdrop]::Clicks -eq ($before + 1)) '2D ear gap inside the character bounds passes a physical click through'
    $legTop = Screen-Point 0.4 0.82; $legBottom = Screen-Point 1.2 0.50
    $legSearch = [System.Drawing.Rectangle]::FromLTRB($legTop[0], $legTop[1], $legBottom[0], $legBottom[1])
    $legGap = [PixelHitNative]::Gap($first, $legSearch, 3)
    $before = [PixelHitBackdrop]::Clicks
    [PixelHitNative]::Click(($rectangle.Left + $legGap.X), ($rectangle.Top + $legGap.Y), $backdrop)
    Start-Sleep -Milliseconds 150
    Assert-Test ([PixelHitBackdrop]::Clicks -eq ($before + 1)) '3D leg gap inside the character bounds passes a physical click through'

    $startBefore = Count-Log 'drag start instance='
    $releaseBefore = Count-Log 'drag release instance='
    $pixelBefore = Count-Log 'hit_region mode=pixel'
    Assert-Test ([PixelHitNative]::Hit($anchorX, $anchorY) -eq $scene) 'Backdrop clicks did not raise it over the character'
    [void][PixelHitNative]::SetCursorPos($anchorX, $anchorY)
    Start-Sleep -Milliseconds 150
    Assert-Test ([PixelHitNative]::Hit($anchorX, $anchorY) -eq $scene) 'Drag press is guarded against foreign windows'
    $mouseDown = $true; [PixelHitNative]::Mouse(2)
    Wait-Log 'drag start instance=' ($startBefore + 1)
    Wait-Log 'hit_region mode=full-window' 1
    Assert-Test (-not [PixelHitNative]::HasRegion($scene)) 'Drag temporarily clears the restrictive region'
    $distance = [int][Math]::Round(0.75 * $rectangle.Height / 4.5)
    foreach ($step in 1..8) {
        $x = $anchorX + [int][Math]::Round($distance * $step / 8.0)
        Assert-Test ([PixelHitNative]::Hit($x, $anchorY) -eq $scene) 'Drag path stays in the owned scene'
        if (-not [PixelHitNative]::SetCursorPos($x, $anchorY)) { throw 'Cannot move drag cursor' }
        Start-Sleep -Milliseconds 45
    }
    [PixelHitNative]::Mouse(4); $mouseDown = $false
    Wait-Log 'drag release instance=' ($releaseBefore + 1)
    Wait-Log 'hit_region mode=pixel' ($pixelBefore + 1)
    $startLine = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') | Select-String 'drag start instance=') | Select-Object -Last 1
    $releaseLine = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') | Select-String 'drag release instance=') | Select-Object -Last 1
    $start = [regex]::Match([string]$startLine, 'drag start instance=(\d+) x=(-?[0-9.]+)')
    $release = [regex]::Match([string]$releaseLine, 'drag release instance=(\d+) x=(-?[0-9.]+)')
    Assert-Test ($start.Success -and $release.Success -and $start.Groups[1].Value -eq $release.Groups[1].Value) 'Press and release target the same character'
    $delta = [double]$release.Groups[2].Value - [double]$start.Groups[2].Value
    Assert-Test ($delta -gt 0.6 -and $delta -lt 0.9) ('Default-mode drag moves the character by ' + [Math]::Round($delta, 3) + ' world units')
    Assert-Test ([PixelHitNative]::HasRegion($scene)) 'Release restores the native character region'
    $before = [PixelHitBackdrop]::Clicks
    [PixelHitNative]::Click($emptyX, $emptyY, $backdrop)
    Start-Sleep -Milliseconds 150
    Assert-Test ([PixelHitBackdrop]::Clicks -eq ($before + 1)) 'Release restores empty-area pass-through'
    Assert-Test ([PixelHitNative]::Hit(($anchorX + $distance), $anchorY) -eq $scene) 'Repeated backdrop clicks preserve scene z-order'
    $box = [PixelHitNative]::RegionBox($scene)
    Assert-Test ($box.Left -ge 0 -and $box.Top -ge 0 -and $box.Right -le $rectangle.Width -and $box.Bottom -le $rectangle.Height -and $box.Width -gt 0 -and $box.Height -gt 0) 'Region union is nonempty and bounded by the client'
    foreach ($sample in 1..4) {
        Start-Sleep -Milliseconds 220
        $path = Capture-Scene ('scene-animation-' + $sample + '.png')
        [PixelHitNative]::VisibleCharacters($path)
        Assert-Test (-not [PixelHitNative]::PassesThrough($scene)) 'Animation sample keeps native hit regions without fallback'
    }
    # Screenshots above establish visible presence/composition, not same-pose
    # completeness. Frozen-frame geometry tests establish coverage separately.
    Toggle-Regions
    Wait-Log 'hit_region mode=interactive' 1
    $before = [PixelHitBackdrop]::Clicks
    [PixelHitNative]::Click($emptyX, $emptyY, $scene)
    Start-Sleep -Milliseconds 150
    Assert-Test ([PixelHitBackdrop]::Clicks -eq $before) 'Turning regions off intercepts empty-area clicks'
    Toggle-Regions
    $before = [PixelHitBackdrop]::Clicks
    [PixelHitNative]::Click($emptyX, $emptyY, $backdrop)
    Start-Sleep -Milliseconds 150
    Assert-Test ([PixelHitBackdrop]::Clicks -eq ($before + 1)) 'Turning regions back on restores empty-area pass-through'
    Assert-Test ((Count-Log 'hit_region mode=fallback') -eq 0) 'No hit-region fallback occurred during the physical-input test'
    Write-Output ('WINDOWS_RESULT pass=true artifacts=' + $run)
} finally {
    try {
        if ($mouseDown) { [PixelHitNative]::Mouse(4) }
        [PixelHitBackdrop]::Shut()
        if ($process -and -not $process.HasExited) {
            if ($scene -ne [IntPtr]::Zero) { [void][PixelHitNative]::PostMessage($scene, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
            if (-not $process.WaitForExit(1500)) { Stop-Process -Id $process.Id -Force -Confirm:$false }
        }
    } finally {
        [void][PixelHitNative]::SetCursorPos($cursor.X, $cursor.Y)
    }
}
