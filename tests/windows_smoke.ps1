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
        Mouse(2); Mouse(4);
    }
    public static void Mouse(uint flags) {
        var input = new Input { Type=0, Data=new InputUnion { Mouse=new MouseInput { Flags=flags } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send test mouse input");
    }
    public static void Key(ushort key, bool release) {
        var input = new Input { Type=1, Data=new InputUnion { Keyboard=new KeyboardInput { Key=key, Flags=release ? 2u : 0u } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send test keyboard input");
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

function Toggle-SceneInput([IntPtr]$Controls) {
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
    [DeliciumWindowTest]::Click($checkX, $checkY)
    Start-Sleep -Milliseconds 250
}

[void][DeliciumWindowTest]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
$cursor = [DeliciumWindowTest+Point]::new()
[void][DeliciumWindowTest]::GetCursorPos([ref]$cursor)
$process = $null
$scene = [IntPtr]::Zero
try {
    # Hide the app's own console window: it is product noise, not overlay
    # under test, and a visible one can cover the scene corner during capture.
    $process = Start-Process -FilePath $Executable -ArgumentList '--exit-after', '30' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError (Join-Path $run 'stderr.log')
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
    # Explicitly lift the owned backdrop into the topmost band: the form's
    # constructor TopMost was not reliably in effect at capture time, and a
    # plain backdrop is pierced by the harness console and foreign windows.
    # The scene is raised after it and stays above, revealing the backdrop
    # through its transparent corners.
    [DeliciumWindowTest]::Raise($backdrop)
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

    Toggle-SceneInput $controls
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
    Toggle-SceneInput $controls
    $before = [DeliciumBackdrop]::Clicks
    [DeliciumWindowTest]::Click($actorX, $actorY)
    Start-Sleep -Milliseconds 200
    Assert-Test ([DeliciumBackdrop]::Clicks -gt $before) 'Overlapping controls can restore click-through after scene focus'
    Write-Output ('WINDOWS_RESULT pass=true artifacts=' + $run)
} finally {
    [DeliciumBackdrop]::Shut()
    if ($process -and -not $process.HasExited) {
        if ($scene -ne [IntPtr]::Zero) { [void][DeliciumWindowTest]::PostMessage($scene, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
        if (-not $process.WaitForExit(1500)) { Stop-Process -Id $process.Id -Force -Confirm:$false }
    }
    [void][DeliciumWindowTest]::SetCursorPos($cursor.X, $cursor.Y)
}
