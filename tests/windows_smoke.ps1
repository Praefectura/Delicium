param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/debug/delicium.exe'),
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$artifacts = Join-Path $project '.artifacts'
$run = Join-Path $artifacts ('windows-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff'))
[void][System.IO.Directory]::CreateDirectory($run)

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$references = [System.IO.Directory]::GetFiles((Join-Path $PSHOME 'ref'), '*.dll') + @(
    [System.Windows.Forms.Form].Assembly.Location,
    [System.Windows.Forms.FormBorderStyle].Assembly.Location,
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

public static class DeliciumWindowTest {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] public struct MouseInput { public int X, Y; public uint Data, Flags, Time; public UIntPtr Extra; }
    [StructLayout(LayoutKind.Sequential)] public struct KeyboardInput { public ushort Key, Scan; public uint Flags, Time; public UIntPtr Extra; }
    [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
        [FieldOffset(0)] public MouseInput Mouse;
        [FieldOffset(0)] public KeyboardInput Keyboard;
    }
    [StructLayout(LayoutKind.Sequential)] public struct Input { public uint Type; public InputUnion Data; }
    public delegate bool Enumerate(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")] public static extern bool EnumWindows(Enumerate callback, IntPtr parameter);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, System.Text.StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr window, out Rect rectangle);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] public static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint code, uint mapType);

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
        if (!GetClientRect(window, out rectangle) || !ClientToScreen(window, ref origin)) throw new Exception("Cannot inspect test window");
        return new Rectangle(origin.X, origin.Y, rectangle.Right, rectangle.Bottom);
    }
    public static void Raise(IntPtr window) {
        if (!SetWindowPos(window, new IntPtr(-1), 0, 0, 0, 0, 0x0013)) throw new Exception("Cannot raise test window");
    }
    public static void Move(IntPtr window, int x, int y) {
        if (!SetWindowPos(window, new IntPtr(-1), x, y, 0, 0, 0x0011)) throw new Exception("Cannot move test window");
    }
    public static IntPtr Hit(int x, int y) {
        return GetAncestor(WindowFromPoint(new Point { X=x, Y=y }), 2);
    }
    public static void Click(int x, int y) {
        if (!SetCursorPos(x,y)) throw new Exception("Cannot move test cursor");
        // Let the move settle so the target window tracks the pointer before
        // the press/release pair arrives; back-to-back injection occasionally
        // produced a click that widgets ignored.
        Thread.Sleep(40);
        Mouse(2); Thread.Sleep(60); Mouse(4);
    }
    public static void Mouse(uint flags) {
        var input = new Input { Type=0, Data=new InputUnion { Mouse=new MouseInput { Flags=flags } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send test mouse input");
    }
    public static void Key(ushort key, bool release) {
        var input = new Input { Type=1, Data=new InputUnion { Keyboard=new KeyboardInput { Key=key, Scan=(ushort)MapVirtualKeyW(key, 0), Flags=release ? 2u : 0u } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send test keyboard input");
    }
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] private static extern IntPtr GetWindowLongPtrW(IntPtr window, int index);
    public static bool PassesThrough(IntPtr window) {
        return (GetWindowLongPtrW(window, -20).ToInt64() & 0x20) != 0;
    }
    public static void Capture(IntPtr scene, IntPtr controls, IntPtr backdrop, string path) {
        var rectangle = Client(scene);
        foreach (var point in new [] { new Point {X=rectangle.Left+2,Y=rectangle.Top+2}, new Point {X=rectangle.Right-3,Y=rectangle.Bottom-3} }) {
            var hit = Hit(point.X, point.Y);
            if (hit != scene && hit != controls && hit != backdrop) {
                uint owner; GetWindowThreadProcessId(hit, out owner);
                throw new Exception(string.Format("Capture aborted: point ({0},{1}) covered by hwnd {2} of pid {3}", point.X, point.Y, hit, owner));
            }
        }
        using (var bitmap = new Bitmap(rectangle.Width, rectangle.Height)) {
            using (var graphics = Graphics.FromImage(bitmap)) graphics.CopyFromScreen(rectangle.Location, System.Drawing.Point.Empty, rectangle.Size);
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
    public static string Corner(string path) {
        using (var bitmap = new Bitmap(path)) {
            var color = bitmap.GetPixel(6, 6);
            if (Math.Abs(color.R-36)>3 || Math.Abs(color.G-58)>3 || Math.Abs(color.B-78)>3) throw new Exception("Transparent corner did not reveal the owned backdrop: " + color);
            return color.ToString();
        }
    }
}

public sealed class DeliciumBackdrop : Form {
    private static readonly ManualResetEvent Ready = new ManualResetEvent(false);
    private static DeliciumBackdrop Instance;
    private static int clicks;
    private DeliciumBackdrop(Rectangle rectangle) {
        FormBorderStyle = FormBorderStyle.None; StartPosition = FormStartPosition.Manual;
        AutoScaleMode = AutoScaleMode.None; Bounds = rectangle; TopMost = true;
        BackColor = Color.FromArgb(36,58,78);
        MouseDown += (sender, args) => Interlocked.Increment(ref clicks);
        Shown += (sender, args) => Ready.Set();
    }
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams {
        get {
            var parameters = base.CreateParams;
            // A click on an activatable window raises it within its band; this
            // backdrop lives in the topmost band to stay above foreign windows
            // like a maximized browser, so it must never be raised by the
            // click-through test clicks. WS_EX_NOACTIVATE keeps it clickable
            // (mouse messages still arrive) but pinned below scene+controls.
            parameters.ExStyle |= 0x08000000; // WS_EX_NOACTIVATE
            return parameters;
        }
    }
    public static IntPtr Open(Rectangle rectangle) {
        var thread = new Thread(() => {
            DeliciumWindowTest.SetThreadDpiAwarenessContext(new IntPtr(-4));
            Instance = new DeliciumBackdrop(rectangle);
            Application.Run(Instance);
        });
        thread.IsBackground=true; thread.SetApartmentState(ApartmentState.STA); thread.Start();
        if (!Ready.WaitOne(5000)) throw new Exception("Backdrop did not become ready");
        return Instance.Handle;
    }
    public static int Clicks { get { return Volatile.Read(ref clicks); } }
    public static void Shut() {
        if (Instance != null && !Instance.IsDisposed) Instance.BeginInvoke(new Action(() => Instance.Close()));
    }
    public static void AnchorBelow(IntPtr window) {
        if (Instance == null || Instance.IsDisposed) throw new Exception("Backdrop is not open");
        Instance.Invoke(new Action(() => {
            // Cross-thread WS_EX_TOPMOST changes are not honored by this
            // window manager, so rejoin the topmost band from the form's own
            // thread and slot the backdrop directly below the given window.
            Instance.TopMost = false;
            Instance.TopMost = true;
            DeliciumWindowTest.SetWindowPos(Instance.Handle, window, 0, 0, 0, 0, 0x0013);
        }));
    }
}
'@

if ($ValidateOnly) {
    Write-Output '[OK] Windows smoke harness compiled without launching windows'
    return
}

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    Write-Output ('[OK] ' + $Message)
}

function Toggle-SceneInput([IntPtr]$Controls, [IntPtr]$Scene) {
    $rectangle = [DeliciumWindowTest]::Client($Controls)
    Assert-Test ([DeliciumWindowTest]::Hit($rectangle.Left + 18, $rectangle.Top + 12) -eq $Controls) 'Control window remains above the scene'
    [DeliciumWindowTest]::Click($rectangle.Left + 18, $rectangle.Top + 12)
    Start-Sleep -Milliseconds 120
    Assert-Test ([DeliciumWindowTest]::GetForegroundWindow() -eq $Controls) 'Physical click can focus the control window'
    # Click the "Scene click-through" checkbox row directly. The control
    # layout is 380 logical pixels wide, so the physical scale follows the
    # measured client width; the checkbox sits at logical (12, 67).
    $scale = $rectangle.Width / 380.0
    $checkX = $rectangle.Left + [int](12 * $scale)
    $checkY = $rectangle.Top + [int](67 * $scale)
    Assert-Test ([DeliciumWindowTest]::Hit($checkX, $checkY) -eq $Controls) 'Click-through checkbox is not covered by a foreign window'
    # The toggle is applied on the app's event loop; wait for the scene's
    # WS_EX_TRANSPARENT bit to actually flip instead of guessing a delay.
    # The app also re-raises the control window on every toggle.
    $wasPassing = [DeliciumWindowTest]::PassesThrough($Scene)
    [DeliciumWindowTest]::Click($checkX, $checkY)
    $deadline = [DateTime]::Now.AddSeconds(3)
    while ([DeliciumWindowTest]::PassesThrough($Scene) -eq $wasPassing -and [DateTime]::Now -lt $deadline) { Start-Sleep -Milliseconds 50 }
    Assert-Test ([DeliciumWindowTest]::PassesThrough($Scene) -ne $wasPassing) 'Click-through toggle reaches the scene window'
    Start-Sleep -Milliseconds 150
}

[void][DeliciumWindowTest]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
$cursor = [DeliciumWindowTest+Point]::new()
[void][DeliciumWindowTest]::GetCursorPos([ref]$cursor)
$process = $null
$scene = [IntPtr]::Zero
try {
    # Hide the app's own console window: it is product noise, not overlay
    # under test, and a visible one can cover the scene corner during capture.
    $process = Start-Process -FilePath $Executable -ArgumentList '--exit-after', '120' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError (Join-Path $run 'stderr.log')
    $deadline = [DateTime]::Now.AddSeconds(15)
    do {
        if ($process.HasExited) { throw "Delicium exited before windows became ready; see $run" }
        $scene = [DeliciumWindowTest]::Find($process.Id, 'Delicium Scene')
        $controls = [DeliciumWindowTest]::Find($process.Id, 'Delicium Controls')
        if ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) 'Both native windows exist'
    $rectangle = [DeliciumWindowTest]::Client($scene)
    $backdrop = [DeliciumBackdrop]::Open($rectangle)
    # Lift the owned backdrop into the topmost band from its own thread
    # (cross-thread WS_EX_TOPMOST changes are silently dropped by this window
    # manager) and slot it directly below the scene. The scene sits above it
    # and reveals the backdrop through its transparent corners; foreign
    # normal windows like a maximized browser stay underneath both.
    [DeliciumWindowTest]::Raise($scene)
    [DeliciumWindowTest]::Raise($controls)
    [DeliciumBackdrop]::AnchorBelow($scene)
    [DeliciumWindowTest]::Raise($scene)
    [DeliciumWindowTest]::Raise($controls)
    Start-Sleep -Seconds 2
    $first = Join-Path $run 'scene-first.png'
    [DeliciumWindowTest]::Capture($scene, $controls, $backdrop, $first)
    Write-Output ('transparent_corner=' + [DeliciumWindowTest]::Corner($first))
    Start-Sleep -Milliseconds 700
    [DeliciumWindowTest]::Capture($scene, $controls, $backdrop, (Join-Path $run 'scene-later.png'))

    $actorX = $rectangle.Left + [int]((0.5 - 2.4 / (4.5 * $rectangle.Width / $rectangle.Height)) * $rectangle.Width)
    $actorY = $rectangle.Top + [int]((3.6 - 0.15) / 4.5 * $rectangle.Height)
    $before = [DeliciumBackdrop]::Clicks
    [DeliciumWindowTest]::Click($actorX, $actorY)
    Start-Sleep -Milliseconds 200
    Assert-Test ([DeliciumBackdrop]::Clicks -gt $before) 'Enabled click-through delivers a physical click to the owned backdrop'

    Toggle-SceneInput $controls $scene
    $before = [DeliciumBackdrop]::Clicks
    [DeliciumWindowTest]::Click($actorX, $actorY)
    Start-Sleep -Milliseconds 200
    Assert-Test ([DeliciumBackdrop]::Clicks -eq $before) 'Disabled click-through intercepts a physical character click'

    $controlRectangle = [DeliciumWindowTest]::Client($controls)
    [DeliciumWindowTest]::Move($controls, $rectangle.Right - $controlRectangle.Width - 24, $rectangle.Top + 30)
    Start-Sleep -Milliseconds 200
    # Focus the scene while the control window overlaps it.
    [DeliciumWindowTest]::Click($actorX, $actorY)
    Start-Sleep -Milliseconds 200
    Toggle-SceneInput $controls $scene
    $before = [DeliciumBackdrop]::Clicks
    [DeliciumWindowTest]::Click($actorX, $actorY)
    Start-Sleep -Milliseconds 200
    Assert-Test ([DeliciumBackdrop]::Clicks -gt $before) 'Overlapping controls can restore click-through after scene focus'

    # ---- GUI coverage: drag, add, reload, remove, typed load, failure preserved ----
    # Drives the control window with physical clicks and the scene with a real
    # press-move-release drag. Assertions use the app's lifecycle log lines
    # (spawn / replace / remove / drag) as causal evidence. Widget positions
    # were measured from captures; all remain inside this app's own windows.
    function Count-Log([string]$Pattern) {
        $lines = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') -ErrorAction SilentlyContinue)
        return @($lines | Select-String -Pattern $Pattern).Count
    }
    function Wait-Increase([string]$Pattern, [int]$Baseline, [int]$Delta, [int]$TimeoutMs) {
        $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
        while ([DateTime]::Now -lt $deadline -and (Count-Log $Pattern) -lt ($Baseline + $Delta)) {
            Start-Sleep -Milliseconds 120
        }
        return (Count-Log $Pattern) -ge ($Baseline + $Delta)
    }
    function Click-Control([double]$LogicalX, [double]$LogicalY, [string]$Label) {
        $rect = [DeliciumWindowTest]::Client($controls)
        $scale = $rect.Width / 380.0
        $x = $rect.Left + [int][Math]::Round($LogicalX * $scale)
        $y = $rect.Top + [int][Math]::Round($LogicalY * $scale)
        Assert-Test ([DeliciumWindowTest]::Hit($x, $y) -eq $controls) ($Label + ' is not covered by a foreign window')
        [DeliciumWindowTest]::Click($x, $y)
        Start-Sleep -Milliseconds 250
    }
    function Get-FieldRow {
        $rect = [DeliciumWindowTest]::Client($controls)
        $scale = $rect.Width / 380.0
        foreach ($point in @(@(($rect.Left + 6), ($rect.Top + 6)), @(([int]($rect.Left + $rect.Width / 2)), ([int]($rect.Top + $rect.Height / 2))), @((($rect.Left + $rect.Width - 8)), (($rect.Top + $rect.Height - 8))))) {
            if ([DeliciumWindowTest]::Hit($point[0], $point[1]) -ne $controls) { throw 'Control window covered during capture' }
        }
        $bitmap = New-Object System.Drawing.Bitmap($rect.Width, $rect.Height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($rect.Location, [System.Drawing.Point]::Empty, $rect.Size)
            $top = -1; $bottom = -1; $x0 = 0; $x1 = 0
            for ($y = 200; $y -lt ($rect.Height - 100); $y++) {
                $count = 0; $rowX0 = -1; $rowX1 = -1
                for ($x = 0; $x -lt $rect.Width; $x++) {
                    $color = $bitmap.GetPixel($x, $y)
                    if ($color.R -lt 25 -and $color.G -lt 25 -and $color.B -lt 25) { $count++; if ($rowX0 -lt 0) { $rowX0 = $x }; $rowX1 = $x }
                }
                if ($count -gt 200) {
                    if ($top -lt 0) { $top = $y }
                    $bottom = $y; $x0 = $rowX0; $x1 = $rowX1
                } elseif ($top -ge 0 -and ($y - $bottom) -gt 4) { break }
            }
            if ($top -lt 0) { throw 'Path field not found in the control window' }
            return @{ LogicalX = (($x0 + $x1) / 2) / $scale; LogicalY = (($top + $bottom) / 2) / $scale }
        } finally {
            $bitmap.Dispose(); $graphics.Dispose()
        }
    }
    # Character-by-character injection loses characters to the app's message
    # coalescing (WM_CHAR must already be queued when the key-down is
    # dispatched), so text enters the field via the clipboard: focus the
    # field, select all, paste. Verified end-to-end against this app.
    function Set-FieldText([string]$Text, [hashtable]$Field) {
        Click-Control $Field.LogicalX $Field.LogicalY 'path field'
        Assert-Test ([DeliciumWindowTest]::GetForegroundWindow() -eq $controls) 'Control window holds focus for pasting'
        Set-Clipboard -Value $Text
        Start-Sleep -Milliseconds 150
        [DeliciumWindowTest]::Key(0x11, $false); Start-Sleep -Milliseconds 100
        [DeliciumWindowTest]::Key(0x41, $false); Start-Sleep -Milliseconds 100
        [DeliciumWindowTest]::Key(0x41, $true);  Start-Sleep -Milliseconds 100
        [DeliciumWindowTest]::Key(0x11, $true);  Start-Sleep -Milliseconds 200
        [DeliciumWindowTest]::Key(0x11, $false); Start-Sleep -Milliseconds 100
        [DeliciumWindowTest]::Key(0x56, $false); Start-Sleep -Milliseconds 100
        [DeliciumWindowTest]::Key(0x56, $true);  Start-Sleep -Milliseconds 100
        [DeliciumWindowTest]::Key(0x11, $true);  Start-Sleep -Milliseconds 250
    }

    # Drag: the overlay must receive mouse input, so disable click-through
    # first (this is the third toggle overall; it starts enabled here).
    Toggle-SceneInput $controls $scene
    Start-Sleep -Milliseconds 250
    $sceneRectangle = [DeliciumWindowTest]::Client($scene)
    $probe = Join-Path $run 'scene-drag-before.png'
    [DeliciumWindowTest]::Capture($scene, $controls, $backdrop, $probe)
    $bitmap = [System.Drawing.Bitmap]::FromFile($probe)
    $anchor = $null; $best = 0
    try {
        for ($y = 640; $y -le 820; $y += 2) {
            for ($x = 170; $x -le 330; $x += 2) {
                $color = $bitmap.GetPixel($x, $y)
                $distance = [Math]::Max([Math]::Abs($color.R - 36), [Math]::Max([Math]::Abs($color.G - 58), [Math]::Abs($color.B - 78)))
                if ($distance -gt $best) { $best = $distance; $anchor = @($x, $y) }
            }
        }
    } finally {
        $bitmap.Dispose()
    }
    Assert-Test ($null -ne $anchor -and $best -gt 100) 'Character pixels are available for the drag anchor'
    $anchorX = $sceneRectangle.Left + $anchor[0]
    $anchorY = $sceneRectangle.Top + $anchor[1]
    Assert-Test ([DeliciumWindowTest]::Hit($anchorX, $anchorY) -eq $scene) 'Drag anchor is not covered by a foreign window'
    $dragBaseline = Count-Log 'drag start instance='
    [void][DeliciumWindowTest]::SetCursorPos($anchorX, $anchorY)
    Start-Sleep -Milliseconds 150
    [DeliciumWindowTest]::Mouse(2)
    Start-Sleep -Milliseconds 180
    foreach ($step in 1..8) {
        [void][DeliciumWindowTest]::SetCursorPos($anchorX + 20 * $step, $anchorY)
        Start-Sleep -Milliseconds 45
    }
    [DeliciumWindowTest]::Mouse(4)
    Start-Sleep -Milliseconds 350
    Assert-Test ((Count-Log 'drag start instance=') -gt $dragBaseline) 'Pressing a character starts a drag'
    Assert-Test ((Count-Log 'drag release instance=') -gt $dragBaseline) 'Releasing the press completes the drag'
    $startLine = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') | Select-String 'drag start instance=') | Select-Object -Last 1
    $releaseLine = @(Get-Content -LiteralPath (Join-Path $run 'stderr.log') | Select-String 'drag release instance=') | Select-Object -Last 1
    $startMatch = [regex]::Match([string]$startLine, 'drag start instance=(\d+) x=(-?[0-9.]+)')
    $releaseMatch = [regex]::Match([string]$releaseLine, 'drag release instance=(\d+) x=(-?[0-9.]+)')
    Assert-Test ($startMatch.Success -and $releaseMatch.Success) 'Drag log lines are parseable'
    Assert-Test ($startMatch.Groups[1].Value -eq $releaseMatch.Groups[1].Value) 'The drag moves a single character'
    $dragDelta = [double]$releaseMatch.Groups[2].Value - [double]$startMatch.Groups[2].Value
    Assert-Test ($dragDelta -gt 0.6 -and $dragDelta -lt 0.9) ('The drag moves the character right by ' + [Math]::Round($dragDelta, 2) + ' world units')

    # Add buttons spawn immediately from the loaded examples.
    $spawnBaseline = Count-Log 'spawn instance='
    Click-Control 34 101 'Add 2D'
    Click-Control 91 101 'Add 3D'
    Start-Sleep -Milliseconds 300
    Assert-Test ((Count-Log 'spawn instance=') -eq ($spawnBaseline + 2)) 'Add 2D and Add 3D each spawn one character'

    # Select the first list row, then reload it.
    Click-Control 30 149 'character list row 1'
    $field = Get-FieldRow
    $replaceBaseline = Count-Log 'replace instance=1\b'
    Click-Control 57 ($field.LogicalY - 48) 'Reload selected'
    Assert-Test (Wait-Increase 'replace instance=1\b' $replaceBaseline 1 4000) 'Reload selected replaces the selected character'

    # Pasted local path: Load as new spawns a new character.
    $copy = Join-Path $run 'copy-2d.dmodel.json'
    Copy-Item -LiteralPath (Join-Path $project 'assets\examples\puppet2d.dmodel.json') -Destination $copy -Force
    $field = Get-FieldRow
    Set-FieldText $copy $field
    $spawnBaseline = Count-Log 'spawn instance='
    Click-Control 47 ($field.LogicalY + 23) 'Load as new'
    Assert-Test (Wait-Increase 'spawn instance=' $spawnBaseline 1 5000) 'Load as new spawns the pasted model'

    # Pasted invalid path: Replace selected must fail and keep the character.
    $invalid = Join-Path $run 'invalid-2d.dmodel.json'
    Set-Content -LiteralPath $invalid -Value '{"format": "delicium.model", "format_version":' -NoNewline
    $field = Get-FieldRow
    Set-FieldText $invalid $field
    $replaceBaseline = Count-Log 'replace instance='
    Click-Control 147 ($field.LogicalY + 23) 'Replace selected'
    Assert-Test (Wait-Increase 'invalid-2d' 0 1 5000) 'A failed replace reports an error for the rejected model'
    Start-Sleep -Milliseconds 300
    Assert-Test ((Count-Log 'replace instance=') -eq $replaceBaseline) 'A failed replace does not replace any character'

    # Remove selected still works on the same character: it survived the
    # failed replace. (It is instance 1, re-selected via list row 1.)
    $field = Get-FieldRow
    $removeBaseline = Count-Log 'remove instance='
    Click-Control 169 ($field.LogicalY - 48) 'Remove selected'
    Assert-Test (Wait-Increase 'remove instance=1\b' $removeBaseline 1 4000) 'Remove selected removes the character that survived the failed replace'

    Write-Output ('WINDOWS_RESULT pass=true artifacts=' + $run)
} finally {
    [DeliciumBackdrop]::Shut()
    if ($process -and -not $process.HasExited) {
        if ($scene -ne [IntPtr]::Zero) { [void][DeliciumWindowTest]::PostMessage($scene, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
        if (-not $process.WaitForExit(1500)) { Stop-Process -Id $process.Id -Force -Confirm:$false }
    }
    [void][DeliciumWindowTest]::SetCursorPos($cursor.X, $cursor.Y)
}
