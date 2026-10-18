param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/debug/delicium.exe'),
    [switch]$ValidateOnly
)

# Action-playback acceptance: the default ordinary launch (bounded
# `--overlay-size 960x640`, no --smoke-test/--bench/--no-wander) keeps
# autonomous wandering enabled, so the two 2D and two 3D textured examples
# exercise idle -> walk -> idle, a physical click that interrupts a walk with
# a priority reaction, a repeat click that restarts the reaction inside its
# one-second window, and a guarded drag that suspends walking until release.
#
# Event matching is offset-scoped: every input records the current stderr
# line count first, and only lines written afterwards are inspected, so an
# earlier walk state or an already-recorded event can never be mistaken for
# this attempt. Screenshots are taken only through the owned backdrop with
# the same fail-closed guards as windows_smoke.ps1 (owner checks, a
# primary-monitor-span rejection and a foreign-window grid sweep), and every
# capture path is printed for manual review. The harness never touches the
# system taskbar or display settings and cleans up its own process in
# `finally`.
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$run = Join-Path $project ('.artifacts/windows-actions-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff') + '-' + [Guid]::NewGuid().ToString('N'))
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
using System.Text;
using System.Threading;
using System.Windows.Forms;

public static class ActionsNative {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct MouseInput { public int X, Y; public uint Data, Flags, Time; public UIntPtr Extra; }
    [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MouseInput Mouse; }
    [StructLayout(LayoutKind.Sequential)] public struct Input { public uint Type; public InputUnion Data; }
    public delegate bool Enumerate(IntPtr window, IntPtr parameter);

    [DllImport("user32.dll")] private static extern bool EnumWindows(Enumerate callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowText(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr window, out Rect rectangle);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] private static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("user32.dll")] private static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] private static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] private static extern IntPtr GetWindowLongPtrW(IntPtr window, int index);
    [DllImport("gdi32.dll")] private static extern IntPtr CreateRectRgn(int left, int top, int right, int bottom);
    [DllImport("gdi32.dll")] private static extern bool DeleteObject(IntPtr region);
    [DllImport("user32.dll")] private static extern int GetWindowRgn(IntPtr window, IntPtr region);

    public static IntPtr Find(uint process, string title) {
        IntPtr result = IntPtr.Zero;
        EnumWindows((window, unused) => {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner != process) return true;
            var text = new StringBuilder(256);
            GetWindowText(window, text, text.Capacity);
            if (text.ToString() == title) result = window;
            return true;
        }, IntPtr.Zero);
        return result;
    }
    public static uint Owner(IntPtr window) { uint process; GetWindowThreadProcessId(window, out process); return process; }
    public static Rectangle Client(IntPtr window) {
        Rect rectangle; Point origin = new Point();
        if (!GetClientRect(window, out rectangle) || !ClientToScreen(window, ref origin)) throw new Exception("Cannot inspect owned client");
        return new Rectangle(origin.X, origin.Y, rectangle.Right, rectangle.Bottom);
    }
    public static void Raise(IntPtr window) {
        if (!SetWindowPos(window, new IntPtr(-1), 0, 0, 0, 0, 0x0013)) throw new Exception("Cannot raise owned window");
    }
    public static IntPtr Hit(int x, int y) { return GetAncestor(WindowFromPoint(new Point { X=x, Y=y }), 2); }
    public static bool LeftButtonDown() { return (GetAsyncKeyState(0x01) & 0x8000) != 0; }
    public static void Mouse(uint flags) {
        var input = new Input { Type=0, Data=new InputUnion { Mouse=new MouseInput { Flags=flags } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send mouse input");
    }
    public static bool PassesThrough(IntPtr window) { return (GetWindowLongPtrW(window, -20).ToInt64() & 0x20) != 0; }
    public static bool HasRegion(IntPtr window) {
        var region = CreateRectRgn(0, 0, 0, 0);
        if (region == IntPtr.Zero) throw new Exception("Cannot allocate region probe");
        try { return GetWindowRgn(window, region) != 0; } finally { DeleteObject(region); }
    }
    // Reject desktop-sized captures, including a multi-monitor bounding box.
    // Only the bounded scene this harness launches may ever be captured.
    public static bool SpansPrimaryMonitor(Rectangle rectangle) {
        var primary = Screen.PrimaryScreen;
        if (primary == null) return false;
        var bounds = primary.Bounds;
        return rectangle.Left <= bounds.Left + 8 && rectangle.Top <= bounds.Top + 8
            && rectangle.Right >= bounds.Right - 8 && rectangle.Bottom >= bounds.Bottom - 8;
    }
    private static bool Background(Color color) { return Math.Abs(color.R-36)<=6 && Math.Abs(color.G-58)<=6 && Math.Abs(color.B-78)<=6; }
    public static void Capture(IntPtr scene, IntPtr controls, IntPtr backdrop, uint appPid, string path) {
        uint owner;
        GetWindowThreadProcessId(scene, out owner);
        if (owner != appPid) throw new Exception("Capture aborted: scene owner changed");
        GetWindowThreadProcessId(controls, out owner);
        if (owner != appPid) throw new Exception("Capture aborted: control owner changed");
        GetWindowThreadProcessId(backdrop, out owner);
        if (owner != (uint)System.Diagnostics.Process.GetCurrentProcess().Id) throw new Exception("Capture aborted: backdrop owner changed");
        var rectangle = Client(scene);
        if (SpansPrimaryMonitor(rectangle)) throw new Exception("Capture aborted: scene client covers the primary monitor (capture only the bounded scene)");
        for (int y=2; y<rectangle.Height; y=Math.Min(y+32, rectangle.Height-1)) {
            for (int x=2; x<rectangle.Width; x=Math.Min(x+32, rectangle.Width-1)) {
                var hit = Hit(rectangle.Left+x, rectangle.Top+y);
                if (hit != scene && hit != controls && hit != backdrop) throw new Exception("Capture aborted: foreign window covers the scene grid");
                if (x == rectangle.Width-1) break;
            }
            if (y == rectangle.Height-1) break;
        }
        using (var bitmap = new Bitmap(rectangle.Width, rectangle.Height)) {
            using (var graphics = Graphics.FromImage(bitmap)) graphics.CopyFromScreen(rectangle.Location, System.Drawing.Point.Empty, rectangle.Size);
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
    // Character presence: count sampled pixels that differ from the owned
    // backdrop color and require the transparent corner to reveal it. This is
    // a presence probe, not a pixel golden.
    public static int Samples(string path) {
        using (var bitmap = new Bitmap(path)) {
            var corner = bitmap.GetPixel(6, 6);
            if (!Background(corner)) throw new Exception("Scene corner did not reveal the owned backdrop: " + corner);
            int count = 0;
            for (int y=0; y<bitmap.Height; y+=3) for (int x=0; x<bitmap.Width; x+=3) {
                var color = bitmap.GetPixel(x, y);
                int distance = Math.Max(Math.Abs(color.R-36), Math.Max(Math.Abs(color.G-58), Math.Abs(color.B-78)));
                if (distance > 40) count++;
            }
            return count;
        }
    }
    // Auxiliary pose-difference probe between two captures of the same scene.
    public static int Difference(string first, string second) {
        using (var left = new Bitmap(first)) using (var right = new Bitmap(second)) {
            if (left.Width != right.Width || left.Height != right.Height) throw new Exception("Capture sizes differ");
            int count = 0;
            for (int y=0; y<left.Height; y+=3) for (int x=0; x<left.Width; x+=3) {
                var a = left.GetPixel(x, y); var b = right.GetPixel(x, y);
                int d = Math.Max(Math.Abs(a.R-b.R), Math.Max(Math.Abs(a.G-b.G), Math.Abs(a.B-b.B)));
                if (d > 24) count++;
            }
            return count;
        }
    }
}

public sealed class ActionsBackdrop : Form {
    private static readonly ManualResetEvent Ready = new ManualResetEvent(false);
    private static ActionsBackdrop Instance;
    private static int clicks;
    private ActionsBackdrop(Rectangle rectangle) {
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
            ActionsNative.SetThreadDpiAwarenessContext(new IntPtr(-4));
            Instance=new ActionsBackdrop(rectangle); Application.Run(Instance);
        });
        thread.IsBackground=true; thread.SetApartmentState(ApartmentState.STA); thread.Start();
        if (!Ready.WaitOne(5000)) throw new Exception("Backdrop did not become ready");
        return Instance.Handle;
    }
    public static int Clicks { get { return Volatile.Read(ref clicks); } }
    public static void AnchorBelow(IntPtr window) {
        Instance.Invoke(new Action(() => {
            Instance.TopMost=false; Instance.TopMost=true;
            if (!ActionsNative.SetWindowPos(Instance.Handle,window,0,0,0,0,0x0013)) throw new Exception("Cannot anchor owned backdrop");
        }));
    }
    public static void Shut() {
        if (Instance!=null && !Instance.IsDisposed) Instance.BeginInvoke(new Action(() => Instance.Close()));
    }
}
'@

if ($ValidateOnly) {
    Write-Output '[OK] Actions harness compiled without launching windows'
    return
}

$invariant = [System.Globalization.CultureInfo]::InvariantCulture
$script:logPath = Join-Path $run 'stderr.log'
$script:run = $run
$script:process = $null
$script:scene = [IntPtr]::Zero
$script:controls = [IntPtr]::Zero
$script:backdrop = [IntPtr]::Zero
$script:rectangle = $null
$script:span = 0.0
$script:bottom = 0.0
$script:mouseDown = $false
$script:captures = @()
$script:sampleThreshold = 150
$script:diffThreshold = 60

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    Write-Output ('[OK] ' + $Message)
}

function Get-LogLines {
    return @(Get-Content -LiteralPath $script:logPath -ErrorAction SilentlyContinue)
}

function Count-Log([string]$Pattern) {
    return @((Get-LogLines) | Select-String -Pattern $Pattern).Count
}

function Wait-LogCount([string]$Pattern, [int]$Count, [int]$TimeoutMs = 4000) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    while ((Count-Log $Pattern) -lt $Count -and [DateTime]::Now -lt $deadline) {
        Start-Sleep -Milliseconds 50
    }
    return (Count-Log $Pattern) -ge $Count
}

function Wait-Log([string]$Pattern, [int]$Count, [int]$TimeoutMs = 4000) {
    Assert-Test (Wait-LogCount $Pattern $Count $TimeoutMs) ('Log observed: ' + $Pattern)
}

function Read-ActionEvents([int]$From = 0) {
    $lines = Get-LogLines
    $events = @()
    for ($index = $From; $index -lt $lines.Count; $index++) {
        $match = [regex]::Match([string]$lines[$index], 'action change instance=(\d+) action=(idle|walk|reaction) restart=(true|false)')
        if ($match.Success) {
            $events += [pscustomobject]@{
                Index = $index
                Id = [int]$match.Groups[1].Value
                Action = $match.Groups[2].Value
                Restart = ($match.Groups[3].Value -eq 'true')
            }
        }
    }
    return $events
}

function Read-WanderEvents([int]$From = 0) {
    $lines = Get-LogLines
    $events = @()
    for ($index = $From; $index -lt $lines.Count; $index++) {
        $line = [string]$lines[$index]
        $start = [regex]::Match($line, 'wander start instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?) y=(-?[0-9]+(?:\.[0-9]+)?) target=(-?[0-9]+(?:\.[0-9]+)?)')
        if ($start.Success) {
            $events += [pscustomobject]@{
                Index = $index; Kind = 'start'; Id = [int]$start.Groups[1].Value
                X = [double]::Parse($start.Groups[2].Value, $invariant)
                Y = [double]::Parse($start.Groups[3].Value, $invariant)
                Target = [double]::Parse($start.Groups[4].Value, $invariant)
            }
            continue
        }
        $arrive = [regex]::Match($line, 'wander arrive instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?) y=(-?[0-9]+(?:\.[0-9]+)?)')
        if ($arrive.Success) {
            $events += [pscustomobject]@{
                Index = $index; Kind = 'arrive'; Id = [int]$arrive.Groups[1].Value
                X = [double]::Parse($arrive.Groups[2].Value, $invariant)
                Y = [double]::Parse($arrive.Groups[3].Value, $invariant)
                Target = $null
            }
        }
    }
    return $events
}

function Read-PokeEvents([int]$From = 0) {
    $lines = Get-LogLines
    $events = @()
    for ($index = $From; $index -lt $lines.Count; $index++) {
        $match = [regex]::Match([string]$lines[$index], 'poke instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?) y=(-?[0-9]+(?:\.[0-9]+)?)')
        if ($match.Success) {
            $events += [pscustomobject]@{ Index = $index; Id = [int]$match.Groups[1].Value }
        }
    }
    return $events
}

function Wait-ActionMatch([int]$From, [int]$Id, [string]$Action, $Restart, [int]$TimeoutMs) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    do {
        $match = @(Read-ActionEvents $From | Where-Object {
            $_.Id -eq $Id -and $_.Action -eq $Action -and ($null -eq $Restart -or $_.Restart -eq $Restart)
        } | Select-Object -First 1)
        if ($match.Count -gt 0) { return $match[0] }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::Now -lt $deadline)
    return $null
}

function Wait-PokeEvent([int]$From, [int]$TimeoutMs) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    do {
        $match = @(Read-PokeEvents $From | Select-Object -First 1)
        if ($match.Count -gt 0) { return $match[0] }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::Now -lt $deadline)
    return $null
}

# Finds an instance that just started a fresh walk of at least $MinRemaining
# world units. Only lines written after the last poll are inspected, so the
# returned start is at most about one poll interval old and the instance is
# still close to its logged start position.
function Find-WalkCandidate([int[]]$Ids, [double]$MinRemaining, [int]$TimeoutMs) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    do {
        $scanFrom = (Get-LogLines).Count
        Start-Sleep -Milliseconds 160
        $from = [Math]::Max(0, $scanFrom - 400)
        $wanders = @(Read-WanderEvents $from)
        $actions = @(Read-ActionEvents $from)
        foreach ($id in $Ids) {
            $walk = @($actions | Where-Object { $_.Id -eq $id -and $_.Action -eq 'walk' -and $_.Index -ge $scanFrom } | Select-Object -First 1)
            if ($walk.Count -eq 0) { continue }
            $start = @($wanders | Where-Object { $_.Kind -eq 'start' -and $_.Id -eq $id -and $_.Index -lt $walk[0].Index } | Select-Object -Last 1)
            if ($start.Count -eq 0) { continue }
            $arrived = @($wanders | Where-Object { $_.Kind -eq 'arrive' -and $_.Id -eq $id -and $_.Index -gt $start[0].Index })
            if ($arrived.Count -gt 0) { continue }
            $later = @($actions | Where-Object { $_.Id -eq $id -and $_.Index -gt $walk[0].Index })
            if ($later.Count -gt 0) { continue }
            $remaining = [Math]::Abs([double]$start[0].Target - [double]$start[0].X)
            if ($remaining -lt $MinRemaining) { continue }
            return [pscustomobject]@{
                Id = $id
                X = [double]$start[0].X
                Target = [double]$start[0].Target
                StartIndex = $start[0].Index
                WalkIndex = $walk[0].Index
                Remaining = $remaining
            }
        }
    } while ([DateTime]::Now -lt $deadline)
    return $null
}

function Screen-Point([double]$X, [double]$Y) {
    $width = [double]$script:rectangle.Width
    $height = [double]$script:rectangle.Height
    $halfWidth = $script:span * 0.5 * $width / $height
    $viewTop = $script:bottom + $script:span
    return [int[]]@(
        [int][Math]::Round((0.5 + $X / (2.0 * $halfWidth)) * $width),
        [int][Math]::Round(($viewTop - $Y) / $script:span * $height)
    )
}

function Ensure-InputTarget([int]$X, [int]$Y) {
    if ($null -eq $script:process -or $script:process.HasExited) { throw 'Physical input guard: launched process is not alive' }
    if ([ActionsNative]::Owner($script:scene) -ne $script:process.Id) { throw 'Physical input guard: scene owner changed' }
    if ([ActionsNative]::Hit($X, $Y) -ne $script:scene) { throw 'Physical input guard: target is not the launched scene' }
}

function Ensure-InputGuard([int]$X, [int]$Y) {
    Ensure-InputTarget $X $Y
    $actual = [ActionsNative+Point]::new()
    if (-not [ActionsNative]::GetCursorPos([ref]$actual) -or $actual.X -ne $X -or $actual.Y -ne $Y) {
        throw 'Physical input guard: cursor moved before injected input'
    }
}

# Probes a small grid around the logged position for a point that the pixel
# hit region actually hands to the scene. The instance may have advanced a
# fraction of a world unit since the log line; offsets stay well inside the
# walk tolerance of a couple of hundred milliseconds.
function Find-Anchor([int]$Id, [double]$WorldX) {
    # The examples overlap in their lower halves. Probe the robot's upper
    # body above the sprite silhouette; the accepted drag ID is still checked.
    if ($Id -in $ids3d) { $heights = @(1.8, 1.65, 1.5, 1.35) }
    elseif ($Id -in $ids2d) { $heights = @(0.4, 0.65, 0.9, 1.05, 0.2) }
    else { throw 'Cannot probe an unknown instance kind' }
    foreach ($worldY in $heights) {
        foreach ($dx in @(0.0, -0.12, 0.12, -0.24, 0.24, -0.36, 0.36)) {
            $point = Screen-Point ($WorldX + $dx) $worldY
            $x = $script:rectangle.Left + $point[0]
            $y = $script:rectangle.Top + $point[1]
            if ($x -le $script:rectangle.Left + 1 -or $x -ge $script:rectangle.Right - 1) { continue }
            if ($y -le $script:rectangle.Top + 1 -or $y -ge $script:rectangle.Bottom - 1) { continue }
            if ([ActionsNative]::Hit($x, $y) -eq $script:scene) { return [int[]]@($x, $y) }
        }
    }
    return $null
}

function Capture-Scene([string]$Name) {
    $path = Join-Path $script:run $Name
    [ActionsNative]::Capture($script:scene, $script:controls, $script:backdrop, $script:process.Id, $path)
    $script:captures += $path
    return $path
}

if ([ActionsNative]::SetThreadDpiAwarenessContext([IntPtr]::new(-4)) -eq [IntPtr]::Zero) {
    throw 'Cannot enable physical-pixel DPI coordinates'
}
$cursor = [ActionsNative+Point]::new()
if (-not [ActionsNative]::GetCursorPos([ref]$cursor)) {
    throw 'Cannot save the cursor position before testing'
}
$passed = $false
try {
    if (-not (Test-Path -LiteralPath $Executable)) {
        throw "Executable does not exist: $Executable"
    }
    # Ordinary bounded launch: no --smoke-test/--bench/--no-wander, so the
    # characters still wander (only those modes normalize wandering off) and
    # every capture stays inside the app's own 960x640 logical window. The
    # user's saved configuration must not leak into the test scene.
    $env:DELICIUM_NO_CONFIG = '1'
    $script:process = Start-Process -FilePath $Executable -ArgumentList '--overlay-size', '960x640', '--exit-after', '240' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError $script:logPath
    $deadline = [DateTime]::Now.AddSeconds(15)
    do {
        if ($script:process.HasExited) { throw ('Application exited before readiness; see ' + $run) }
        $script:scene = [ActionsNative]::Find($script:process.Id, 'Delicium Scene')
        $script:controls = [ActionsNative]::Find($script:process.Id, 'Delicium Controls')
        if ($script:scene -ne [IntPtr]::Zero -and $script:controls -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($script:scene -ne [IntPtr]::Zero -and $script:controls -ne [IntPtr]::Zero -and $script:scene -ne $script:controls) 'PID-scoped scene and control windows exist with independent handles'
    Assert-Test (([ActionsNative]::Owner($script:scene) -eq $script:process.Id) -and ([ActionsNative]::Owner($script:controls) -eq $script:process.Id)) 'Both window handles belong to the launched process'
    Wait-Log 'wander mode=on' 1 15000
    Wait-Log 'spawn instance=' 4 15000
    Wait-Log 'action change instance=' 4 15000
    Wait-Log 'scene_view ' 1 15000
    Wait-Log 'wander bounds left=' 1 15000
    Wait-Log 'hit_region mode=pixel' 1 15000
    Wait-Log 'drag release instance=' 0 0

    # ---- startup: spawn classes, initial bound actions, bounded mapping ----
    $spawns = @()
    foreach ($line in Get-LogLines) {
        $match = [regex]::Match([string]$line, 'spawn instance=(\d+) asset=(\d+) kind=(\w+)')
        if ($match.Success) { $spawns += [pscustomobject]@{ Id = [int]$match.Groups[1].Value; Kind = $match.Groups[3].Value } }
    }
    $ids2d = @($spawns | Where-Object { $_.Kind -eq 'sprite2d' } | ForEach-Object { $_.Id } | Sort-Object)
    $ids3d = @($spawns | Where-Object { $_.Kind -eq 'skeleton3d' } | ForEach-Object { $_.Id } | Sort-Object)
    Assert-Test ($spawns.Count -eq 4 -and $ids2d.Count -eq 2 -and $ids3d.Count -eq 2) 'Default launch spawns two 2D and two 3D textured examples'
    $initialActions = @(Read-ActionEvents)
    foreach ($id in @($ids2d) + @($ids3d)) {
        $hits = @($initialActions | Where-Object { $_.Id -eq $id -and $_.Action -eq 'idle' -and -not $_.Restart })
        Assert-Test ($hits.Count -ge 1) ('Spawn records an initial bound idle action for instance ' + $id)
    }

    $script:rectangle = [ActionsNative]::Client($script:scene)
    Assert-Test ($script:rectangle.Width -gt 0 -and $script:rectangle.Height -gt 0) 'Scene client has a positive physical size'
    Assert-Test (-not [ActionsNative]::SpansPrimaryMonitor($script:rectangle)) 'Bounded scene does not cover the primary monitor'
    $viewLine = @(Get-LogLines | Select-String 'scene_view ') | Select-Object -Last 1
    $viewMatch = [regex]::Match([string]$viewLine, 'scene_view mode=(\w+) span=(-?[0-9]+(?:\.[0-9]+)?) bottom=(-?[0-9]+(?:\.[0-9]+)?)')
    Assert-Test ($viewMatch.Success -and $viewMatch.Groups[1].Value -eq 'bounded') 'Ordinary bounded launch records a parseable bounded scene view'
    $script:span = [double]::Parse($viewMatch.Groups[2].Value, $invariant)
    $script:bottom = [double]::Parse($viewMatch.Groups[3].Value, $invariant)
    Assert-Test ($script:span -gt 0) 'Scene view span is positive'
    $boundsLine = @(Get-LogLines | Select-String 'wander bounds left=') | Select-Object -Last 1
    $boundsMatch = [regex]::Match([string]$boundsLine, 'wander bounds left=(-?[0-9]+(?:\.[0-9]+)?) right=(-?[0-9]+(?:\.[0-9]+)?) margin=([0-9]+(?:\.[0-9]+)?)')
    Assert-Test $boundsMatch.Success 'Wander bounds log is parseable'
    $left = [double]::Parse($boundsMatch.Groups[1].Value, $invariant)
    $right = [double]::Parse($boundsMatch.Groups[2].Value, $invariant)
    $expectedHalf = $script:span * 0.5 * $script:rectangle.Width / $script:rectangle.Height
    Assert-Test ([Math]::Abs($left + $expectedHalf) -lt 0.05 -and [Math]::Abs($right - $expectedHalf) -lt 0.05) 'Wander bounds match the parsed scene view and client aspect'

    # ---- own backdrop for guarded captures, never the desktop ----
    $script:backdrop = [ActionsBackdrop]::Open($script:rectangle)
    Assert-Test (([ActionsNative]::Owner($script:backdrop) -ne 0) -and ([ActionsNative]::Owner($script:backdrop) -ne $script:process.Id)) 'Capture backdrop is owned by this test process, not the application'
    [ActionsNative]::Raise($script:scene); [ActionsNative]::Raise($script:controls)
    [ActionsBackdrop]::AnchorBelow($script:scene)
    [ActionsNative]::Raise($script:scene); [ActionsNative]::Raise($script:controls)
    Start-Sleep -Milliseconds 500
    $emptyX = $script:rectangle.Left + 40; $emptyY = $script:rectangle.Top + 40
    Assert-Test ([ActionsNative]::Hit($emptyX, $emptyY) -eq $script:backdrop) 'An empty scene area reveals the owned backdrop'

    # ---- natural cycles: idle -> walk -> idle for both classes ----
    function Find-Cycle($ActionEvents, $WanderEvents, [int]$Id) {
        $forId = @($ActionEvents | Where-Object { $_.Id -eq $Id })
        $wanders = @($WanderEvents | Where-Object { $_.Id -eq $Id })
        for ($i = 0; $i + 2 -lt $forId.Count; $i++) {
            if ($forId[$i].Action -ne 'idle' -or $forId[$i + 1].Action -ne 'walk' -or $forId[$i + 2].Action -ne 'idle') { continue }
            $idle = $forId[$i]; $walk = $forId[$i + 1]; $idle2 = $forId[$i + 2]
            $start = @($wanders | Where-Object { $_.Kind -eq 'start' -and $_.Index -gt $idle.Index -and $_.Index -lt $walk.Index } | Select-Object -Last 1)
            if ($start.Count -eq 0) { continue }
            $arrive = @($wanders | Where-Object { $_.Kind -eq 'arrive' -and $_.Index -gt $walk.Index -and $_.Index -lt $idle2.Index } | Select-Object -First 1)
            if ($arrive.Count -eq 0) { continue }
            if ([Math]::Abs([double]$start[0].Target - [double]$start[0].X) -lt 0.25) { continue }
            if ([Math]::Abs([double]$arrive[0].X - [double]$start[0].X) -lt 0.2) { continue }
            return [pscustomobject]@{ Id = $Id; Idle = $idle; Start = $start[0]; Walk = $walk; Arrive = $arrive[0]; Idle2 = $idle2 }
        }
        return $null
    }
    $cycleDeadline = [DateTime]::Now.AddSeconds(60)
    $cycle2d = $null; $cycle3d = $null
    do {
        $allActions = @(Read-ActionEvents)
        $allWanders = @(Read-WanderEvents)
        if ($null -eq $cycle2d) {
            foreach ($id in $ids2d) {
                $cycle2d = Find-Cycle $allActions $allWanders $id
                if ($null -ne $cycle2d) { break }
            }
        }
        if ($null -eq $cycle3d) {
            foreach ($id in $ids3d) {
                $cycle3d = Find-Cycle $allActions $allWanders $id
                if ($null -ne $cycle3d) { break }
            }
        }
        if ($null -eq $cycle2d -or $null -eq $cycle3d) { Start-Sleep -Milliseconds 120 }
    } while (($null -eq $cycle2d -or $null -eq $cycle3d) -and [DateTime]::Now -lt $cycleDeadline)
    Assert-Test ($null -ne $cycle2d) 'A 2D instance completed idle -> walk -> idle with a start/arrive displacement'
    Assert-Test ($null -ne $cycle3d) 'A 3D instance completed idle -> walk -> idle with a start/arrive displacement'
    Write-Output ('cycle 2D instance=' + $cycle2d.Id + ' walk ' + [Math]::Round($cycle2d.Start.X, 3) + ' -> ' + [Math]::Round($cycle2d.Arrive.X, 3))
    Write-Output ('cycle 3D instance=' + $cycle3d.Id + ' walk ' + [Math]::Round($cycle3d.Start.X, 3) + ' -> ' + [Math]::Round($cycle3d.Arrive.X, 3))

    # ---- idle captures for both classes ----
    function Capture-Idle([int[]]$Ids, [string]$Name) {
        $deadline = [DateTime]::Now.AddSeconds(20)
        do {
            $scanFrom = (Get-LogLines).Count
            Start-Sleep -Milliseconds 120
            $fresh = @(Read-ActionEvents $scanFrom | Where-Object { $_.Action -eq 'idle' -and -not $_.Restart -and $_.Id -in $Ids })
            if ($fresh.Count -eq 0) { continue }
            $id = $fresh[0].Id
            $latest = @(Read-ActionEvents | Where-Object { $_.Id -eq $id } | Select-Object -Last 1)
            if ($latest[0].Action -ne 'idle') { continue }
            $captureFrom = (Get-LogLines).Count
            $path = Capture-Scene $Name
            $after = @(Read-ActionEvents $captureFrom | Where-Object { $_.Id -eq $id })
            if ($after.Count -gt 0) { continue }
            # Diagnostics must not become part of the returned instance ID.
            Assert-Test ([ActionsNative]::Samples($path) -gt $script:sampleThreshold) ('Idle frame shows visible characters: ' + $path) | Out-Host
            return $id
        } while ([DateTime]::Now -lt $deadline)
        return 0
    }
    Assert-Test ((Capture-Idle $ids2d 'scene-idle-2d.png') -ne 0) 'Idle 2D capture succeeded inside a real idle pause'
    Assert-Test ((Capture-Idle $ids3d 'scene-idle-3d.png') -ne 0) 'Idle 3D capture succeeded inside a real idle pause'

    # ---- 2D walk phases: two different frames of one fresh walk ----
    $walkCandidate2d = Find-WalkCandidate $ids2d 1.5 30000
    Assert-Test ($null -ne $walkCandidate2d) 'A 2D instance started a fresh walk long enough for phase captures'
    $walkA2d = Capture-Scene 'scene-walk-a-2d.png'
    Assert-Test ([ActionsNative]::Samples($walkA2d) -gt $script:sampleThreshold) ('Walk phase A shows visible characters: ' + $walkA2d)
    Start-Sleep -Milliseconds 350
    $still = @(Read-ActionEvents $walkCandidate2d.WalkIndex | Where-Object { $_.Id -eq $walkCandidate2d.Id -and $_.Action -ne 'walk' })
    $stillArrived = @(Read-WanderEvents $walkCandidate2d.WalkIndex | Where-Object { $_.Kind -eq 'arrive' -and $_.Id -eq $walkCandidate2d.Id })
    Assert-Test ($still.Count -eq 0 -and $stillArrived.Count -eq 0) 'The 2D walk is still in flight between the phase captures'
    $walkB2d = Capture-Scene 'scene-walk-b-2d.png'
    Assert-Test ([ActionsNative]::Samples($walkB2d) -gt $script:sampleThreshold) ('Walk phase B shows visible characters: ' + $walkB2d)
    $walkDiff2d = [ActionsNative]::Difference($walkA2d, $walkB2d)
    Assert-Test ($walkDiff2d -gt $script:diffThreshold) ('Walk phases A and B differ by ' + $walkDiff2d + ' samples')

    # ---- 3D walk phases: two different frames of one fresh walk ----
    $walkCandidate3d = Find-WalkCandidate $ids3d 1.5 30000
    Assert-Test ($null -ne $walkCandidate3d) 'A 3D instance started a fresh walk long enough for phase captures'
    $walkA3d = Capture-Scene 'scene-walk-a-3d.png'
    Assert-Test ([ActionsNative]::Samples($walkA3d) -gt $script:sampleThreshold) ('3D walk phase A shows visible characters: ' + $walkA3d)
    Start-Sleep -Milliseconds 350
    $still3d = @(Read-ActionEvents $walkCandidate3d.WalkIndex | Where-Object { $_.Id -eq $walkCandidate3d.Id -and $_.Action -ne 'walk' })
    $stillArrived3d = @(Read-WanderEvents $walkCandidate3d.WalkIndex | Where-Object { $_.Kind -eq 'arrive' -and $_.Id -eq $walkCandidate3d.Id })
    Assert-Test ($still3d.Count -eq 0 -and $stillArrived3d.Count -eq 0) 'The 3D walk is still in flight between the phase captures'
    $walkB3d = Capture-Scene 'scene-walk-b-3d.png'
    Assert-Test ([ActionsNative]::Samples($walkB3d) -gt $script:sampleThreshold) ('3D walk phase B shows visible characters: ' + $walkB3d)
    $walkDiff3d = [ActionsNative]::Difference($walkA3d, $walkB3d)
    Assert-Test ($walkDiff3d -gt $script:diffThreshold) ('3D walk phases A and B differ by ' + $walkDiff3d + ' samples')

    # ---- physical clicks for both model kinds: reaction, repeat and recovery ----
    function Test-ClickReaction([int[]]$Ids, [string]$WalkFrame, [string]$Kind) {
    $clickDeadline = [DateTime]::Now.AddSeconds(50)
    $clicked = $false
    $click = $null
    for ($attempt = 1; $attempt -le 4 -and -not $clicked -and [DateTime]::Now -lt $clickDeadline; $attempt++) {
        $candidate = Find-WalkCandidate $Ids 1.5 12000
        if ($null -eq $candidate) { continue }
        $anchor = Find-Anchor $candidate.Id $candidate.X
        if ($null -eq $anchor) { continue }
        Write-Output ('click candidate kind=' + $Kind + ' instance=' + $candidate.Id + ' attempt=' + $attempt + ' screen=' + $anchor[0] + ',' + $anchor[1])
        $pressFrom = (Get-LogLines).Count
        $dragBefore = Count-Log 'drag start instance='
        $pixelBefore = Count-Log 'hit_region mode=pixel'
        Ensure-InputTarget $anchor[0] $anchor[1]
        if (-not [ActionsNative]::SetCursorPos($anchor[0], $anchor[1])) { continue }
        Start-Sleep -Milliseconds 40
        Ensure-InputGuard $anchor[0] $anchor[1]
        $script:mouseDown = $true
        [ActionsNative]::Mouse(2)
        Start-Sleep -Milliseconds 90
        [ActionsNative]::Mouse(4)
        $script:mouseDown = $false
        if (-not (Wait-LogCount 'drag start instance=' ($dragBefore + 1) 2500)) { continue }
        $dragLine = @(Get-LogLines | Select-String 'drag start instance=') | Select-Object -Last 1
        $dragMatch = [regex]::Match([string]$dragLine, 'drag start instance=(\d+)')
        if (-not $dragMatch.Success -or [int]$dragMatch.Groups[1].Value -ne $candidate.Id) { continue }
        $poke = Wait-PokeEvent $pressFrom 2500
        if ($null -eq $poke -or $poke.Id -ne $candidate.Id) { continue }
        $reaction = Wait-ActionMatch $pressFrom $candidate.Id 'reaction' $false 2500
        if ($null -eq $reaction) { continue }
        # The press suspended the walk (idle) and no arrival ever completed it.
        $suspend = @(Read-ActionEvents $pressFrom | Where-Object { $_.Id -eq $candidate.Id -and $_.Action -eq 'idle' -and $_.Index -lt $reaction.Index } | Select-Object -First 1)
        if ($suspend.Count -eq 0) { continue }
        $walkSeq = @(Read-ActionEvents | Where-Object { $_.Id -eq $candidate.Id -and $_.Action -eq 'walk' -and $_.Index -lt $suspend[0].Index } | Select-Object -Last 1)
        if ($walkSeq.Count -eq 0) { continue }
        $completed = @(Read-WanderEvents | Where-Object { $_.Kind -eq 'arrive' -and $_.Id -eq $candidate.Id -and $_.Index -gt $walkSeq[0].Index -and $_.Index -lt $reaction.Index })
        if ($completed.Count -gt 0) { continue }
        if (-not (Wait-LogCount 'hit_region mode=pixel' ($pixelBefore + 1) 3000)) { continue }
        $latest = @(Read-ActionEvents | Where-Object { $_.Id -eq $candidate.Id } | Select-Object -Last 1)
        if ($latest[0].Action -ne 'reaction') { continue }
        $reactionShot = Capture-Scene ('scene-reaction-' + $Kind + '.png')
        Assert-Test ([ActionsNative]::Samples($reactionShot) -gt $script:sampleThreshold) ('Reaction frame shows visible characters: ' + $reactionShot)
        $reactionDiff = [ActionsNative]::Difference($WalkFrame, $reactionShot)
        Assert-Test ($reactionDiff -gt $script:diffThreshold) ('Reaction frame differs from the recorded walk frame by ' + $reactionDiff + ' samples')
        # Repeat click inside the same one-second reaction window.
        Start-Sleep -Milliseconds 120
        $anchor2 = Find-Anchor $candidate.Id $candidate.X
        if ($null -eq $anchor2) { continue }
        $press2From = (Get-LogLines).Count
        $drag2Before = Count-Log 'drag start instance='
        $pixel2Before = Count-Log 'hit_region mode=pixel'
        Ensure-InputTarget $anchor2[0] $anchor2[1]
        if (-not [ActionsNative]::SetCursorPos($anchor2[0], $anchor2[1])) { continue }
        Start-Sleep -Milliseconds 40
        Ensure-InputGuard $anchor2[0] $anchor2[1]
        $script:mouseDown = $true
        [ActionsNative]::Mouse(2)
        Start-Sleep -Milliseconds 90
        [ActionsNative]::Mouse(4)
        $script:mouseDown = $false
        if (-not (Wait-LogCount 'drag start instance=' ($drag2Before + 1) 2500)) { continue }
        $drag2Line = @(Get-LogLines | Select-String 'drag start instance=') | Select-Object -Last 1
        $drag2Match = [regex]::Match([string]$drag2Line, 'drag start instance=(\d+)')
        if (-not $drag2Match.Success -or [int]$drag2Match.Groups[1].Value -ne $candidate.Id) { continue }
        $poke2 = Wait-PokeEvent $press2From 2500
        if ($null -eq $poke2 -or $poke2.Id -ne $candidate.Id) { continue }
        $reaction2 = Wait-ActionMatch $press2From $candidate.Id 'reaction' $true 2500
        if ($null -eq $reaction2) { continue }
        # No idle between the two reaction events: the restart happened inside
        # the same one-second window.
        $between = @(Read-ActionEvents | Where-Object { $_.Id -eq $candidate.Id -and $_.Action -eq 'idle' -and $_.Index -gt $reaction.Index -and $_.Index -lt $reaction2.Index })
        if ($between.Count -gt 0) { continue }
        if (-not (Wait-LogCount 'hit_region mode=pixel' ($pixel2Before + 1) 3000)) { continue }
        $reactionEnd = Wait-ActionMatch $reaction2.Index $candidate.Id 'idle' $false 3000
        if ($null -eq $reactionEnd) { continue }
        $resumedWalk = Wait-ActionMatch $reactionEnd.Index $candidate.Id 'walk' $null 9000
        if ($null -eq $resumedWalk) { continue }
        $resumedStart = @(Read-WanderEvents | Where-Object { $_.Kind -eq 'start' -and $_.Id -eq $candidate.Id -and $_.Index -gt $reactionEnd.Index -and $_.Index -lt $resumedWalk.Index } | Select-Object -First 1)
        if ($resumedStart.Count -eq 0) { continue }
        $click = [pscustomobject]@{
            Candidate = $candidate; Suspend = $suspend[0]; WalkSeq = $walkSeq[0]
            Reaction = $reaction; Reaction2 = $reaction2
            ReactionEnd = $reactionEnd; ResumedWalk = $resumedWalk; ResumedStart = $resumedStart[0]
            Poke = $poke; Poke2 = $poke2
        }
        $clicked = $true
    }
    Assert-Test $clicked 'A guarded physical click interrupted a walk, restarted on a repeat click and recovered in time'
    Assert-Test ($click.WalkSeq.Index -lt $click.Suspend.Index -and $click.Suspend.Index -lt $click.Reaction.Index) 'The click interrupted an in-flight walk before the reaction started'
    Assert-Test ($click.Reaction.Restart -eq $false -and $click.Poke.Index -lt $click.Reaction.Index) 'The first accepted click logged a poke and a fresh reaction (restart=false)'
    Assert-Test ($click.Reaction2.Restart -eq $true -and $click.Poke2.Index -lt $click.Reaction2.Index) 'A repeat click inside the window restarted the reaction (restart=true)'
    Assert-Test ($click.ReactionEnd.Restart -eq $false) 'The reaction settled back into idle without a restart'
    Assert-Test ($click.ResumedStart.Index -lt $click.ResumedWalk.Index) ('Autonomous walking recovered after the ' + $Kind + ' reaction')
    }
    Test-ClickReaction $ids2d $walkB2d '2d'
    Test-ClickReaction $ids3d $walkB3d '3d'

    # ---- guarded drag: no walk while held, idle first, then recovery ----
    $dragDeadline = [DateTime]::Now.AddSeconds(45)
    $dragged = $false
    for ($attempt = 1; $attempt -le 3 -and -not $dragged -and [DateTime]::Now -lt $dragDeadline; $attempt++) {
        $candidate = Find-WalkCandidate $ids3d 1.2 12000
        if ($null -eq $candidate) { continue }
        $anchor = Find-Anchor $candidate.Id $candidate.X
        if ($null -eq $anchor) { continue }
        $pressFrom = (Get-LogLines).Count
        $dragBefore = Count-Log 'drag start instance='
        $pixelBefore = Count-Log 'hit_region mode=pixel'
        $pokeBefore = Count-Log 'poke instance='
        Ensure-InputTarget $anchor[0] $anchor[1]
        if (-not [ActionsNative]::SetCursorPos($anchor[0], $anchor[1])) { continue }
        Start-Sleep -Milliseconds 40
        Ensure-InputGuard $anchor[0] $anchor[1]
        $script:mouseDown = $true
        [ActionsNative]::Mouse(2)
        if (-not (Wait-LogCount 'drag start instance=' ($dragBefore + 1) 2500)) {
            [ActionsNative]::Mouse(4); $script:mouseDown = $false; continue
        }
        $dragLine = @(Get-LogLines | Select-String 'drag start instance=') | Select-Object -Last 1
        $dragMatch = [regex]::Match([string]$dragLine, 'drag start instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?)')
        if (-not $dragMatch.Success -or [int]$dragMatch.Groups[1].Value -ne $candidate.Id) {
            [ActionsNative]::Mouse(4); $script:mouseDown = $false; continue
        }
        $dragStartX = [double]::Parse($dragMatch.Groups[2].Value, $invariant)
        $suspend = Wait-ActionMatch $pressFrom $candidate.Id 'idle' $null 1500
        if ($null -eq $suspend) {
            [ActionsNative]::Mouse(4); $script:mouseDown = $false; continue
        }
        # A real travel keeps this a drag: move the cursor right in guarded steps.
        foreach ($step in 1..6) {
            $x = $anchor[0] + 10 * $step
            if (-not [ActionsNative]::SetCursorPos($x, $anchor[1])) { throw 'Cannot move the drag cursor' }
            if ([ActionsNative]::Hit($x, $anchor[1]) -ne $script:scene) { throw 'Drag path left the owned scene' }
            Start-Sleep -Milliseconds 60
        }
        $holdDeadline = [DateTime]::Now.AddMilliseconds(2000)
        $holdOk = $true
        while ([DateTime]::Now -lt $holdDeadline) {
            if ($script:process.HasExited) { throw 'Launched process exited during the guarded hold' }
            if (-not [ActionsNative]::LeftButtonDown()) { $holdOk = $false; break }
            if ([ActionsNative]::Owner($script:scene) -ne $script:process.Id) { throw 'Physical input guard: scene owner changed during the hold' }
            Start-Sleep -Milliseconds 200
        }
        if (-not $holdOk) { [ActionsNative]::Mouse(4); $script:mouseDown = $false; continue }
        $holdWalk = @(Read-ActionEvents $pressFrom | Where-Object { $_.Id -eq $candidate.Id -and $_.Action -eq 'walk' })
        $holdStarts = @(Read-WanderEvents $pressFrom | Where-Object { $_.Kind -eq 'start' -and $_.Id -eq $candidate.Id })
        $releaseFrom = (Get-LogLines).Count
        [ActionsNative]::Mouse(4)
        $script:mouseDown = $false
        if (-not (Wait-LogCount 'drag release instance=' ($dragBefore + 1) 2500)) { continue }
        $releaseLine = @(Get-LogLines | Select-String 'drag release instance=') | Select-Object -Last 1
        $releaseMatch = [regex]::Match([string]$releaseLine, 'drag release instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?)')
        if (-not $releaseMatch.Success -or [int]$releaseMatch.Groups[1].Value -ne $candidate.Id) { continue }
        if ($holdWalk.Count -ne 0 -or $holdStarts.Count -ne 0) { continue }
        if (-not (Wait-LogCount 'hit_region mode=pixel' ($pixelBefore + 1) 3000)) { continue }
        if ((Count-Log 'poke instance=') -ne $pokeBefore) { continue }
        Start-Sleep -Milliseconds 900
        $early = @(Read-ActionEvents $releaseFrom | Where-Object { $_.Id -eq $candidate.Id -and $_.Action -eq 'walk' })
        if ($early.Count -gt 0) { continue }
        $resumedWalk = Wait-ActionMatch $releaseFrom $candidate.Id 'walk' $null 9000
        if ($null -eq $resumedWalk) { continue }
        $resumedStart = @(Read-WanderEvents | Where-Object { $_.Kind -eq 'start' -and $_.Id -eq $candidate.Id -and $_.Index -gt $releaseFrom -and $_.Index -lt $resumedWalk.Index } | Select-Object -First 1)
        if ($resumedStart.Count -eq 0) { continue }
        $releaseX = [double]::Parse($releaseMatch.Groups[2].Value, $invariant)
        $worldTravel = $releaseX - $dragStartX
        if ($worldTravel -lt 0.1 -or $worldTravel -gt 0.6) { continue }
        $drag = [pscustomobject]@{
            Candidate = $candidate; Suspend = $suspend
            ResumedStart = $resumedStart[0]; ResumedWalk = $resumedWalk
            Travel = $worldTravel
        }
        $dragged = $true
    }
    Assert-Test $dragged 'A guarded drag suspended a walking instance, held without walking and recovered after release'
    Assert-Test ($drag.Suspend.Index -gt $drag.Candidate.WalkIndex) 'The drag press turned the in-flight walk into idle before the hold'
    Assert-Test ($drag.Travel -gt 0.1 -and $drag.Travel -lt 0.6) ('The drag moved the instance by about the injected travel: ' + [Math]::Round($drag.Travel, 3))
    Assert-Test ($drag.ResumedStart.Index -lt $drag.ResumedWalk.Index) 'Wandering recovered after release with a fresh wander start and walk action'

    # ---- no fallback, region still installed ----
    Assert-Test ((Count-Log 'hit_region mode=fallback') -eq 0) 'Actions acceptance completed without hit-region fallback'
    Assert-Test (-not [ActionsNative]::PassesThrough($script:scene)) 'Scene remains input-active (no whole-window pass-through fallback)'
    Assert-Test ([ActionsNative]::HasRegion($script:scene)) 'Character hit regions remain installed at the end'
    $passed = $true
} finally {
    try {
        if ($script:mouseDown) { [ActionsNative]::Mouse(4) }
        [ActionsBackdrop]::Shut()
        if ($script:process -and -not $script:process.HasExited) {
            if ($script:scene -ne [IntPtr]::Zero -and [ActionsNative]::Owner($script:scene) -eq $script:process.Id) {
                [void][ActionsNative]::PostMessage($script:scene, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
            }
            if (-not $script:process.WaitForExit(1500) -and -not $script:process.HasExited) {
                $script:process.Kill()
                [void]$script:process.WaitForExit(1500)
            }
        }
    } finally {
        [void][ActionsNative]::SetCursorPos($cursor.X, $cursor.Y)
    }
}

if ($passed) {
    Assert-Test ($script:process.HasExited) 'Launched process exited before reporting acceptance success'
    Assert-Test ($script:process.ExitCode -eq 0) ('Launched process exited successfully: code=' + $script:process.ExitCode)
    foreach ($capture in $script:captures) { Write-Output ('screenshot=' + $capture) }
    Write-Output ('WINDOWS_RESULT pass=true artifacts=' + $run)
}
