param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/debug/delicium.exe'),
    [switch]$ValidateOnly
)

# Ordinary-launch wander acceptance. This harness deliberately takes no
# screenshots: it validates the running application through owned windows,
# stderr lifecycle logs and guarded physical input only.
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$run = Join-Path $project ('.artifacts/windows-wander-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff') + '-' + [Guid]::NewGuid().ToString('N'))
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
using System.Runtime.InteropServices;
using System.Text;

public static class WanderNative {
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
    [DllImport("user32.dll")] private static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] private static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")] private static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] private static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);

    public static IntPtr Find(uint process, string title) {
        IntPtr result = IntPtr.Zero;
        EnumWindows((window, unused) => {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner != process) return true;
            var text = new StringBuilder(256);
            GetWindowText(window, text, text.Capacity);
            if (text.ToString() == title) result = window;
            return true;
        }, IntPtr.Zero);
        return result;
    }

    public static uint Owner(IntPtr window) {
        uint process;
        GetWindowThreadProcessId(window, out process);
        return process;
    }

    public static Rect Client(IntPtr window) {
        Rect rectangle;
        Point origin = new Point();
        if (!GetClientRect(window, out rectangle) || !ClientToScreen(window, ref origin))
            throw new Exception("Cannot inspect owned client");
        return new Rect {
            Left = origin.X,
            Top = origin.Y,
            Right = origin.X + rectangle.Right,
            Bottom = origin.Y + rectangle.Bottom
        };
    }

    public static IntPtr Hit(int x, int y) {
        return GetAncestor(WindowFromPoint(new Point { X = x, Y = y }), 2);
    }

    public static bool LeftButtonDown() {
        return (GetAsyncKeyState(0x01) & 0x8000) != 0;
    }

    public static void Mouse(uint flags) {
        var input = new Input {
            Type = 0,
            Data = new InputUnion {
                Mouse = new MouseInput { Flags = flags }
            }
        };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1)
            throw new Exception("Cannot send mouse input");
    }
}
'@

if ($ValidateOnly) {
    Write-Output '[OK] Wander harness compiled without launching windows'
    return
}

$invariant = [System.Globalization.CultureInfo]::InvariantCulture
$logPath = Join-Path $run 'stderr.log'

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    Write-Output ('[OK] ' + $Message)
}

function Get-LogLines {
    return @(Get-Content -LiteralPath $logPath -ErrorAction SilentlyContinue)
}

function Count-Log([string]$Pattern) {
    return @((Get-LogLines) | Select-String -Pattern $Pattern).Count
}

function Wait-Log([string]$Pattern, [int]$Count, [int]$TimeoutMs = 4000) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    while ((Count-Log $Pattern) -lt $Count -and [DateTime]::Now -lt $deadline) {
        Start-Sleep -Milliseconds 50
    }
    Assert-Test ((Count-Log $Pattern) -ge $Count) ('Log observed: ' + $Pattern)
}

function Wait-LogCount([string]$Pattern, [int]$Count, [int]$TimeoutMs = 4000) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    while ((Count-Log $Pattern) -lt $Count -and [DateTime]::Now -lt $deadline) {
        Start-Sleep -Milliseconds 50
    }
    return (Count-Log $Pattern) -ge $Count
}

function Read-WanderEvents {
    $events = @()
    foreach ($line in Get-LogLines) {
        $start = [regex]::Match([string]$line, 'wander start instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?) y=(-?[0-9]+(?:\.[0-9]+)?) target=(-?[0-9]+(?:\.[0-9]+)?)')
        if ($start.Success) {
            $events += [pscustomobject]@{
                Kind = 'start'
                Id = [int]$start.Groups[1].Value
                X = [double]::Parse($start.Groups[2].Value, $invariant)
                Y = [double]::Parse($start.Groups[3].Value, $invariant)
                Target = [double]::Parse($start.Groups[4].Value, $invariant)
                Text = [string]$line
            }
            continue
        }
        $arrive = [regex]::Match([string]$line, 'wander arrive instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?) y=(-?[0-9]+(?:\.[0-9]+)?)')
        if ($arrive.Success) {
            $events += [pscustomobject]@{
                Kind = 'arrive'
                Id = [int]$arrive.Groups[1].Value
                X = [double]::Parse($arrive.Groups[2].Value, $invariant)
                Y = [double]::Parse($arrive.Groups[3].Value, $invariant)
                Target = $null
                Text = [string]$line
            }
        }
    }
    return $events
}

function Count-WanderPairs($Events) {
    $open = @{}
    $pairs = 0
    foreach ($event in @($Events)) {
        if ($event.Kind -eq 'start') {
            $open[$event.Id] = $true
        } elseif ($event.Kind -eq 'arrive' -and $open.ContainsKey($event.Id)) {
            $open.Remove($event.Id)
            $pairs++
        }
    }
    return $pairs
}

function Last-WanderEvent($Events, [int[]]$Ids) {
    return @($Events | Where-Object { $_.Id -in $Ids } | Select-Object -Last 1)[0]
}

function Screen-Point([double]$X, [double]$Y, $Rectangle, [double]$Span, [double]$Bottom, [double]$CenterX) {
    # Materialize every native-struct field as a scalar before arithmetic.
    # PowerShell can otherwise preserve a one-field PSObject array returned
    # through a .NET value type and route `+` to array concatenation.
    $left = [double]$Rectangle.Left
    $top = [double]$Rectangle.Top
    $width = [double]$Rectangle.Right - $left
    $height = [double]$Rectangle.Bottom - $top
    $halfWidth = [double]$Span * 0.5 * $width / $height
    $viewTop = [double]$Bottom + [double]$Span
    $screenX = [int][Math]::Round($left + (0.5 + ([double]$X - $CenterX) / (2.0 * $halfWidth)) * $width)
    $screenY = [int][Math]::Round($top + ($viewTop - [double]$Y) / [double]$Span * $height)
    return [int[]]@($screenX, $screenY)
}

function Assert-OwnHit([IntPtr]$Scene, [int]$X, [int]$Y, [string]$Label) {
    Assert-Test ($null -ne $process -and -not $process.HasExited) ($Label + ' is attempted while the launched process is alive')
    Assert-Test ([WanderNative]::Owner($Scene) -eq $process.Id) ($Label + ' scene handle is still owned by the launched process')
    Assert-Test ([WanderNative]::Hit($X, $Y) -eq $Scene) ($Label + ' belongs to the launched scene window')
}

function Ensure-InputGuard([IntPtr]$Scene, [int]$X, [int]$Y) {
    if ($null -eq $process -or $process.HasExited) { throw 'Physical input guard: launched process is not alive' }
    if ([WanderNative]::Owner($Scene) -ne $process.Id) { throw 'Physical input guard: scene owner changed' }
    $actual = [WanderNative+Point]::new()
    if (-not [WanderNative]::GetCursorPos([ref]$actual) -or $actual.X -ne $X -or $actual.Y -ne $Y) {
        throw 'Physical input guard: cursor moved before injected input'
    }
    if ([WanderNative]::Hit($X, $Y) -ne $Scene) { throw 'Physical input guard: target is not the launched scene' }
}

function Place-WanderingCharacter([int]$TargetX, [int]$TargetY) {
    $deadline = [DateTime]::Now.AddSeconds(15)
    $sample = $null
    do {
        $candidate = Last-WanderEvent @(Read-WanderEvents) ([int[]](1, 2))
        if ($null -ne $candidate) {
            $point = Screen-Point $candidate.X $candidate.Y $rectangle $span $bottom $centerX
            if ([WanderNative]::Hit($point[0], $point[1]) -eq $scene) {
                [void][WanderNative]::SetCursorPos($point[0], $point[1])
                if ([WanderNative]::Hit($point[0], $point[1]) -eq $scene) { $sample = $candidate; break }
            }
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($null -ne $sample) 'A recent wander endpoint is physically pickable for monitor placement'
    Ensure-InputGuard $scene $point[0] $point[1]
    $starts = Count-Log 'drag start instance='
    $releases = Count-Log 'drag release instance='
    [WanderNative]::Mouse(2); $script:mouseDown = $true
    Wait-Log 'drag start instance=' ($starts + 1)
    $startLine = @(Get-LogLines | Select-String 'drag start instance=') | Select-Object -Last 1
    $start = [regex]::Match([string]$startLine, 'drag start instance=(\d+) x=(-?[0-9.]+) y=(-?[0-9.]+)')
    Assert-Test ($start.Success -and [int]$start.Groups[1].Value -eq $sample.Id) 'Monitor placement captures the sampled instance'
    $startX = [double]::Parse($start.Groups[2].Value, $invariant)
    $startY = [double]::Parse($start.Groups[3].Value, $invariant)
    # First cross the seam at the current visible height, then move vertically
    # inside the secondary monitor. Do not send the cursor through a dead zone.
    $fromX = $point[0]; $fromY = $point[1]
    foreach ($waypoint in @(@($TargetX, $fromY), @($TargetX, $TargetY))) {
        foreach ($step in 1..16) {
            $x = $fromX + [int][Math]::Round(($waypoint[0] - $fromX) * $step / 16.0)
            $y = $fromY + [int][Math]::Round(($waypoint[1] - $fromY) * $step / 16.0)
            Assert-OwnHit $scene $x $y 'Monitor-placement drag path'
            [void][WanderNative]::SetCursorPos($x, $y)
            Ensure-InputGuard $scene $x $y
            Start-Sleep -Milliseconds 30
        }
        $fromX = $waypoint[0]; $fromY = $waypoint[1]
    }
    $eventOffset = @(Read-WanderEvents).Count
    [WanderNative]::Mouse(4); $script:mouseDown = $false
    Wait-Log 'drag release instance=' ($releases + 1)
    $releaseLine = @(Get-LogLines | Select-String 'drag release instance=') | Select-Object -Last 1
    $release = [regex]::Match([string]$releaseLine, 'drag release instance=(\d+) x=(-?[0-9.]+) y=(-?[0-9.]+)')
    Assert-Test ($release.Success -and [int]$release.Groups[1].Value -eq $sample.Id) 'Monitor placement releases the same instance'
    $releaseX = [double]::Parse($release.Groups[2].Value, $invariant)
    $releaseY = [double]::Parse($release.Groups[3].Value, $invariant)
    Assert-Test ([Math]::Abs(($releaseX - $startX) - ($TargetX - $point[0]) * $span / $height) -lt 0.05) 'Monitor-placement x displacement uses the anchored view'
    Assert-Test ([Math]::Abs(($releaseY - $startY) + ($TargetY - $point[1]) * $span / $height) -lt 0.05) 'Monitor-placement y displacement uses the anchored view'
    $script:placed = [pscustomobject]@{ Id = $sample.Id; X = $releaseX; Y = $releaseY; Offset = $eventOffset }
}

if ([WanderNative]::SetThreadDpiAwarenessContext([IntPtr]::new(-4)) -eq [IntPtr]::Zero) {
    throw 'Cannot enable physical-pixel DPI coordinates'
}
$cursor = [WanderNative+Point]::new()
if (-not [WanderNative]::GetCursorPos([ref]$cursor)) {
    throw 'Cannot save the cursor position before testing'
}
$process = $null
$scene = [IntPtr]::Zero
$controls = [IntPtr]::Zero
$mouseDown = $false
$passed = $false
try {
    if (-not (Test-Path -LiteralPath $Executable)) {
        throw "Executable does not exist: $Executable"
    }
    # No --overlay-size and no --no-wander: this is the ordinary desktop-span path.
    # The user's saved configuration must not leak into the test scene.
    $env:DELICIUM_NO_CONFIG = '1'
    $process = Start-Process -FilePath $Executable -ArgumentList '--exit-after', '240' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError $logPath
    $deadline = [DateTime]::Now.AddSeconds(15)
    do {
        if ($process.HasExited) { throw ('Application exited before readiness; see ' + $run) }
        $scene = [WanderNative]::Find($process.Id, 'Delicium Scene')
        $controls = [WanderNative]::Find($process.Id, 'Delicium Controls')
        if ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero -and $scene -ne $controls) 'PID-scoped scene and control windows exist with independent handles'
    Assert-Test (([WanderNative]::Owner($scene) -eq $process.Id) -and ([WanderNative]::Owner($controls) -eq $process.Id)) 'Both window handles belong to the launched process'
    Wait-Log 'wander mode=on' 1 15000
    Wait-Log 'spawn instance=' 4 15000
    Wait-Log 'scene_view ' 1 15000
    Wait-Log 'wander bounds left=' 1 15000

    $rectangle = [WanderNative]::Client($scene)
    $width = $rectangle.Right - $rectangle.Left
    $height = $rectangle.Bottom - $rectangle.Top
    Assert-Test ($width -gt 0 -and $height -gt 0) 'Scene client has a positive physical size'
    $viewLine = @(Get-LogLines | Select-String 'scene_view ') | Select-Object -Last 1
    $viewMatch = [regex]::Match([string]$viewLine, 'scene_view mode=(\w+) span=(-?[0-9]+(?:\.[0-9]+)?) bottom=(-?[0-9]+(?:\.[0-9]+)?)')
    Assert-Test $viewMatch.Success 'Scene view log is parseable'
    Assert-Test ($viewMatch.Groups[1].Value -eq 'desktop') 'Wander harness uses ordinary desktop-span mode'
    $span = [double]::Parse($viewMatch.Groups[2].Value, $invariant)
    $bottom = [double]::Parse($viewMatch.Groups[3].Value, $invariant)
    Assert-Test ($span -gt 0) 'Scene view span is positive'
    $desktopLine = @(Get-LogLines | Select-String 'desktop non_fullscreen=true') | Select-Object -Last 1
    $centerMatch = [regex]::Match([string]$desktopLine, 'center_x=(-?[0-9.]+)')
    Assert-Test $centerMatch.Success 'Desktop horizontal center is recorded'
    $centerX = [double]::Parse($centerMatch.Groups[1].Value, $invariant)

    $boundsLine = @(Get-LogLines | Select-String 'wander bounds left=') | Select-Object -Last 1
    $boundsMatch = [regex]::Match([string]$boundsLine, 'wander bounds left=(-?[0-9]+(?:\.[0-9]+)?) right=(-?[0-9]+(?:\.[0-9]+)?) margin=([0-9]+(?:\.[0-9]+)?)')
    Assert-Test $boundsMatch.Success 'Wander bounds log is parseable'
    $left = [double]::Parse($boundsMatch.Groups[1].Value, $invariant)
    $right = [double]::Parse($boundsMatch.Groups[2].Value, $invariant)
    $margin = [double]::Parse($boundsMatch.Groups[3].Value, $invariant)
    Assert-Test ($right -gt $left -and [Math]::Abs(($left + $right) / 2.0 - $centerX) -lt 0.02) 'Wander bounds have positive width and match the desktop horizontal center'
    Assert-Test ($margin -gt 0) 'Wander edge margin is positive'
    $expectedWidth = $span * $width / $height
    Assert-Test ([Math]::Abs(($right - $left) - $expectedWidth) -lt 0.02) ('Wander width matches scene-view aspect: recorded=' + ($right - $left) + ' expected=' + $expectedWidth)
    $walkLeft = $left + $margin
    $walkRight = $right - $margin

    # Let the four ordinary-launch instances produce several complete walks.
    $observeDeadline = [DateTime]::Now.AddSeconds(60)
    do {
        $events = @(Read-WanderEvents)
        if ((Count-WanderPairs $events) -ge 3) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $observeDeadline)
    $events = @(Read-WanderEvents)
    $pairs = Count-WanderPairs $events
    Assert-Test ($pairs -ge 3) ('At least three wander start/arrive pairs observed: ' + $pairs)
    Assert-Test (@($events | Where-Object { $_.Kind -eq 'start' }).Count -ge 3) 'At least three wander starts observed'
    Assert-Test (@($events | Where-Object { $_.Kind -eq 'arrive' }).Count -ge 3) 'At least three wander arrivals observed'
    foreach ($event in $events) {
        Assert-Test ($event.Id -ge 1 -and $event.Id -le 4) ('Wander event instance is one of the four launched ids: ' + $event.Id)
        Assert-Test ([Math]::Abs($event.Y - 0.150) -lt 0.002) ('Wander event keeps y=0.150 for instance ' + $event.Id)
        Assert-Test ($event.X -ge ($walkLeft - 0.05) -and $event.X -le ($walkRight + 0.05)) ('Wander event x stays inside the effective walkable range for instance ' + $event.Id)
        if ($event.Kind -eq 'start') {
            Assert-Test ($event.Target -ge ($walkLeft - 0.05) -and $event.Target -le ($walkRight + 0.05)) ('Wander target stays inside the effective range for instance ' + $event.Id)
        }
    }

    $dragCompleted = $false
    for ($attempt = 1; $attempt -le 3 -and -not $dragCompleted; $attempt++) {
        $events = @(Read-WanderEvents)
        $sample = Last-WanderEvent $events ([int[]](1, 2))
        if ($null -eq $sample) {
            Start-Sleep -Milliseconds 100
            continue
        }
        $point = Screen-Point $sample.X 0.15 $rectangle $span $bottom $centerX
        $x = [int]$point[0]; $y = [int]$point[1]
        if ($x -lt $rectangle.Left -or $x -ge $rectangle.Right -or $y -lt $rectangle.Top -or $y -ge $rectangle.Bottom) {
            Start-Sleep -Milliseconds 100
            continue
        }
        if ([WanderNative]::Hit($x, $y) -ne $scene) {
            Start-Sleep -Milliseconds 150
            continue
        }
        $id = $sample.Id
        $startPattern = 'drag start instance=' + $id + '\b'
        $wanderStartPattern = 'wander start instance=' + $id + '\b'
        $releasePattern = 'drag release instance=' + $id + '\b'
        if (-not [WanderNative]::SetCursorPos($x, $y)) {
            continue
        }
        $actualCursor = [WanderNative+Point]::new()
        if (-not [WanderNative]::GetCursorPos([ref]$actualCursor) -or $actualCursor.X -ne $x -or $actualCursor.Y -ne $y) {
            continue
        }
        Start-Sleep -Milliseconds 150
        if (-not [WanderNative]::GetCursorPos([ref]$actualCursor) -or $actualCursor.X -ne $x -or $actualCursor.Y -ne $y) {
            continue
        }
        if ([WanderNative]::Hit($x, $y) -ne $scene) {
            continue
        }
        Assert-OwnHit $scene $x $y ('Attempt ' + $attempt + ' press point')
        $startBefore = Count-Log $startPattern
        $releaseBefore = Count-Log $releasePattern
        Ensure-InputGuard $scene $x $y
        [WanderNative]::Mouse(2)
        $mouseDown = $true
        Assert-Test ([WanderNative]::LeftButtonDown()) 'Injected left button is held after the guarded press'
        if (-not (Wait-LogCount $startPattern ($startBefore + 1) 2000)) {
            [WanderNative]::Mouse(4)
            $mouseDown = $false
            continue
        }
        $dragStartLine = @(Get-LogLines | Select-String ('drag start instance=' + $id + ' ')) | Select-Object -Last 1
        $dragStartMatch = [regex]::Match([string]$dragStartLine, 'drag start instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?)')
        Assert-Test ($dragStartMatch.Success -and [int]$dragStartMatch.Groups[1].Value -eq $id) 'Guarded press starts a drag for the selected instance'
        $dragStartX = [double]::Parse($dragStartMatch.Groups[2].Value, $invariant)
        Assert-Test ([Math]::Abs($dragStartX - $sample.X) -le 0.05) 'Drag start position matches the selected wander sample'
        $heldStarts = Count-Log $wanderStartPattern
        $holdDeadline = [DateTime]::Now.AddMilliseconds(5500)
        while ([DateTime]::Now -lt $holdDeadline) {
            Ensure-InputGuard $scene $x $y
            if (-not [WanderNative]::LeftButtonDown()) { throw 'Physical input guard: left button was released during the hold' }
            Start-Sleep -Milliseconds 100
        }
        Assert-Test ((Count-Log $wanderStartPattern) -eq $heldStarts) ('Held drag suspends wandering for instance ' + $id)
        Ensure-InputGuard $scene $x $y
        [WanderNative]::Mouse(4)
        $mouseDown = $false
        Wait-Log $releasePattern ($releaseBefore + 1) 2000
        $dragReleaseLine = @(Get-LogLines | Select-String ('drag release instance=' + $id + ' ')) | Select-Object -Last 1
        $dragReleaseMatch = [regex]::Match([string]$dragReleaseLine, 'drag release instance=(\d+) x=(-?[0-9]+(?:\.[0-9]+)?)')
        Assert-Test ($dragReleaseMatch.Success -and [int]$dragReleaseMatch.Groups[1].Value -eq $id) 'Release completes the same owned drag'
        Assert-Test ($dragReleaseMatch.Groups[2].Value -eq $dragStartMatch.Groups[2].Value) 'Release preserves the press x while the cursor is held still'
        $releaseX = [double]::Parse($dragReleaseMatch.Groups[2].Value, $invariant)
        $resumeBefore = @((Read-WanderEvents) | Where-Object { $_.Kind -eq 'start' -and $_.Id -eq $id }).Count
        $resumeDeadline = [DateTime]::Now.AddSeconds(8)
        $resumed = $null
        do {
            $current = @(Read-WanderEvents) | Where-Object { $_.Kind -eq 'start' -and $_.Id -eq $id }
            if (@($current).Count -gt $resumeBefore) {
                $resumed = @($current | Select-Object -Last 1)[0]
                break
            }
            Start-Sleep -Milliseconds 100
        } while ([DateTime]::Now -lt $resumeDeadline)
        Assert-Test ($null -ne $resumed) ('Wandering resumes after release for instance ' + $id)
        Assert-Test ([Math]::Abs($resumed.X - $releaseX) -le 0.05) ('Resumed walk starts near the released x for instance ' + $id)
        $dragCompleted = $true
    }
    Assert-Test $dragCompleted 'A guarded press succeeded within three latest-log retries'

    $primary = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $secondary = @([System.Windows.Forms.Screen]::AllScreens | ForEach-Object { $_.Bounds } | Where-Object {
        $_.Left -eq $primary.Right -and $_.Top -lt ($primary.Top - 100) -and $_.Bottom -gt ($primary.Bottom + 100)
    }) | Select-Object -First 1
    if ($secondary) {
        $seam = $centerX + (($primary.Right - $rectangle.Left) / [double]$width - 0.5) * $expectedWidth
        $sharedY = [int][Math]::Round($primary.Bottom - (0.15 + 0.9) * $height / $span)
        Place-WanderingCharacter ($primary.Right + 40) $sharedY
        $crossId = $placed.Id; $crossOffset = $placed.Offset
        $deadline = [DateTime]::Now.AddSeconds(60)
        $crossed = $false
        do {
            $pending = $null
            foreach ($event in @((Read-WanderEvents) | Select-Object -Skip $crossOffset | Where-Object { $_.Id -eq $crossId })) {
                if ($event.Kind -eq 'start') {
                    $pending = $event
                } elseif ($null -ne $pending -and (($pending.X - $seam) * ($event.X - $seam)) -lt 0 -and [Math]::Abs($event.X - $pending.Target) -lt 0.02) {
                    $crossed = $true
                    Write-Output ('seam walk instance=' + $crossId + ' from=' + $pending.X + ' to=' + $event.X + ' seam=' + $seam)
                    break
                }
            }
            if (-not $crossed) { Start-Sleep -Milliseconds 100 }
        } while (-not $crossed -and [DateTime]::Now -lt $deadline)
        Assert-Test $crossed 'An autonomous start-to-arrival walk crosses the shared monitor seam'

        $secondaryX = [int](($secondary.Left + $secondary.Right) / 2)
        foreach ($probeY in @([int](($secondary.Top + $primary.Top) / 2), [int](($primary.Bottom + $secondary.Bottom) / 2))) {
            Place-WanderingCharacter $secondaryX $probeY
            $probe = $placed
            $segmentLeft = $centerX + (($secondary.Left - $rectangle.Left) / [double]$width - 0.5) * $expectedWidth + $margin
            $segmentRight = $centerX + (($secondary.Right - $rectangle.Left) / [double]$width - 0.5) * $expectedWidth - $margin
            $deadline = [DateTime]::Now.AddSeconds(35)
            do {
                $probeEvents = @((Read-WanderEvents) | Select-Object -Skip $probe.Offset | Where-Object { $_.Id -eq $probe.Id })
                if ((Count-WanderPairs $probeEvents) -ge 1) { break }
                Start-Sleep -Milliseconds 100
            } while ([DateTime]::Now -lt $deadline)
            Assert-Test ((Count-WanderPairs $probeEvents) -ge 1) ('A complete autonomous walk occurs at secondary-only physical y=' + $probeY)
            foreach ($event in $probeEvents) {
                Assert-Test ([Math]::Abs($event.Y - $probe.Y) -lt 0.002) 'Secondary-only wandering preserves the dragged height'
                Assert-Test ($event.X -ge ($segmentLeft - 0.02) -and $event.X -le ($segmentRight + 0.02)) 'Secondary-only wandering stays inside its actual visible horizontal segment'
                if ($event.Kind -eq 'start') {
                    Assert-Test ($event.Target -ge ($segmentLeft - 0.02) -and $event.Target -le ($segmentRight + 0.02)) 'Secondary-only target excludes the primary-column dead zone'
                }
            }
        }
    } else {
        Write-Output '[SKIP] Secondary-only height probes require a right-adjacent monitor extending above and below the primary'
    }
    Assert-Test ((Count-Log 'hit_region mode=fallback') -eq 0) 'Wander acceptance completed without hit-region fallback'
    $passed = $true
} finally {
    try {
        if ($mouseDown) { [WanderNative]::Mouse(4) }
        if ($process -and -not $process.HasExited) {
            if ($scene -ne [IntPtr]::Zero -and [WanderNative]::Owner($scene) -eq $process.Id) {
                [void][WanderNative]::PostMessage($scene, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
            }
            if (-not $process.WaitForExit(1500) -and -not $process.HasExited) {
                $process.Kill()
                [void]$process.WaitForExit(1500)
            }
        }
    } finally {
        [void][WanderNative]::SetCursorPos($cursor.X, $cursor.Y)
    }
}

if ($passed) {
    Assert-Test ($process.HasExited) 'Launched process exited before reporting acceptance success'
    Assert-Test ($process.ExitCode -eq 0) ('Launched process exited successfully: code=' + $process.ExitCode)
    Write-Output ('WINDOWS_RESULT pass=true artifacts=' + $run)
}
