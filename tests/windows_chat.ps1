param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/release/delicium.exe'),
    [switch]$ValidateOnly
)

# Conversation-panel acceptance: launches the engine's pet scene against a
# temporary APPDATA whose chat endpoint is a loopback fixture this script
# owns. The panel is scrolled to, driven with real input, and captured in the
# waiting, replied and local-fallback states. Nothing leaves the machine and
# the user's own configuration is never read or written.
$ErrorActionPreference = 'Stop'
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
using System.Collections.Generic;
using System.Drawing;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class ChatPanelTest {
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
    public static string Text(IntPtr window) {
        var text = new System.Text.StringBuilder(1024);
        GetWindowText(window, text, text.Capacity);
        return text.ToString();
    }
    public static Rectangle Client(IntPtr window) {
        Rect rectangle; Point origin = new Point();
        if (!GetClientRect(window, out rectangle) || !ClientToScreen(window, ref origin)) throw new Exception("Cannot inspect the control window");
        return new Rectangle(origin.X, origin.Y, rectangle.Right, rectangle.Bottom);
    }
    public static void Raise(IntPtr window) {
        // A maximized browser or terminal can cover a normal window set
        // foreground by a background pwsh; the capture needs the topmost band.
        if (!SetWindowPos(window, new IntPtr(-1), 0, 0, 0, 0, 0x0013 | 0x0040)) throw new Exception("Cannot raise the control window");
        Thread.Sleep(300);
    }
    public static void Lower(IntPtr window) {
        if (!SetWindowPos(window, new IntPtr(-2), 0, 0, 0, 0, 0x0013 | 0x0040)) throw new Exception("Cannot lower the control window");
        Thread.Sleep(150);
    }
    public static IntPtr Hit(int x, int y) {
        return GetAncestor(WindowFromPoint(new Point { X=x, Y=y }), 2);
    }
    public static void Click(int x, int y) {
        if (!SetCursorPos(x, y)) throw new Exception("Cannot move the test cursor");
        Thread.Sleep(40);
        Mouse(2); Thread.Sleep(60); Mouse(4);
    }
    public static void Mouse(uint flags) {
        var input = new Input { Type=0, Data=new InputUnion { Mouse=new MouseInput { Flags=flags } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send test mouse input");
    }
    public static void Wheel(int delta) {
        var input = new Input { Type=0, Data=new InputUnion { Mouse=new MouseInput { Data=(uint)delta, Flags=0x0800 } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send the test wheel");
        Thread.Sleep(30);
    }
    public static void Key(ushort key, bool release) {
        var input = new Input { Type=1, Data=new InputUnion { Keyboard=new KeyboardInput { Key=key, Scan=(ushort)MapVirtualKeyW(key, 0), Flags=release ? 2u : 0u } } };
        if (SendInput(1, new [] { input }, Marshal.SizeOf(typeof(Input))) != 1) throw new Exception("Cannot send test keyboard input");
    }
    static void Toggle(ushort key, bool down) {
        Key(key, !down);
        Thread.Sleep(80);
    }
    public static void SelectAll() {
        Toggle(0x11, true); Toggle(0x41, true); Toggle(0x41, false); Toggle(0x11, false);
        Thread.Sleep(120);
    }
    public static void Paste() {
        Toggle(0x11, true); Toggle(0x56, true); Toggle(0x56, false); Toggle(0x11, false);
        Thread.Sleep(200);
    }
    public static void Enter() {
        Toggle(0x0D, true); Toggle(0x0D, false);
    }
    public static void Capture(IntPtr window, string path) {
        var rectangle = Client(window);
        var center = new Point { X = rectangle.Left + rectangle.Width / 2, Y = rectangle.Top + rectangle.Height / 2 };
        if (GetAncestor(WindowFromPoint(center), 2) != window) throw new Exception("Capture aborted: the control window is covered");
        using (var bitmap = new Bitmap(rectangle.Width, rectangle.Height)) {
            using (var graphics = Graphics.FromImage(bitmap)) graphics.CopyFromScreen(rectangle.Location, System.Drawing.Point.Empty, rectangle.Size);
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
    // TextEdit backgrounds use egui's extreme background (grey 10); nothing
    // else on the control page is that dark. Each contiguous band of such rows
    // is one field, so the bottom-most band is the conversation input (the
    // local-path field sits above it).
    public static Rectangle[] Fields(string path) {
        using (var bitmap = new Bitmap(path)) {
            var bands = new List<Rectangle>();
            int top = -1, bottom = -1, x0 = 0, x1 = 0;
            for (int y = 0; y <= bitmap.Height; y++) {
                int count = 0, rowX0 = -1, rowX1 = -1;
                if (y < bitmap.Height) {
                    for (int x = 0; x < bitmap.Width; x++) {
                        var color = bitmap.GetPixel(x, y);
                        if (color.R < 25 && color.G < 25 && color.B < 25) { count++; if (rowX0 < 0) rowX0 = x; rowX1 = x; }
                    }
                }
                if (count > 150) {
                    if (top < 0) { top = y; x0 = rowX0; }
                    bottom = y; x1 = rowX1;
                } else if (top >= 0 && y - bottom > 4) {
                    bands.Add(new Rectangle(x0, top, x1 - x0 + 1, bottom - top + 1));
                    top = -1; bottom = -1;
                }
            }
            return bands.ToArray();
        }
    }
    public static int Differences(string first, string second, int left, int top, int right, int bottom) {
        using (var a = new Bitmap(first)) using (var b = new Bitmap(second)) {
            if (a.Width != b.Width || a.Height != b.Height) throw new Exception("Capture sizes differ");
            int count = 0;
            for (int y = Math.Max(0, top); y < Math.Min(a.Height, bottom); y += 2)
                for (int x = Math.Max(0, left); x < Math.Min(a.Width, right); x += 2) {
                    var firstPixel = a.GetPixel(x, y); var secondPixel = b.GetPixel(x, y);
                    if (Math.Abs(firstPixel.R-secondPixel.R) > 8 || Math.Abs(firstPixel.G-secondPixel.G) > 8 || Math.Abs(firstPixel.B-secondPixel.B) > 8) count++;
                }
            return count;
        }
    }
}

// A minimal OpenAI-compatible responder on 127.0.0.1. It records every
// request in full and answers after an optional delay, so the panel's waiting
// state can be observed before the reply lands.
public sealed class ChatFixture : IDisposable {
    readonly TcpListener listener;
    readonly Thread thread;
    readonly List<string> requests = new List<string>();
    volatile bool running = true;
    public int ResponseDelayMs = 0;
    public string Reply = "Remote fixture reply.";
    public ChatFixture() {
        listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        thread = new Thread(Loop); thread.IsBackground = true; thread.Start();
    }
    public int Port { get { return ((IPEndPoint)listener.LocalEndpoint).Port; } }
    public string[] Requests { get { lock (requests) return requests.ToArray(); } }
    void Loop() {
        while (running) {
            TcpClient client;
            try { client = listener.AcceptTcpClient(); } catch { return; }
            ThreadPool.QueueUserWorkItem(delegate { Handle(client); });
        }
    }
    void Handle(TcpClient client) {
        try {
            using (client) {
                client.ReceiveTimeout = 5000;
                var stream = client.GetStream();
                var reader = new StreamReader(stream, Encoding.ASCII, false, 1024, true);
                string requestLine = reader.ReadLine() ?? "";
                var headers = new StringBuilder();
                int length = 0;
                while (true) {
                    string line = reader.ReadLine();
                    if (line == null || line.Length == 0) break;
                    headers.Append(line).Append("\r\n");
                    if (line.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase)) length = int.Parse(line.Substring(15).Trim());
                }
                var body = new char[length];
                int read = 0;
                while (read < length) {
                    int count = reader.Read(body, read, length - read);
                    if (count <= 0) break;
                    read += count;
                }
                lock (requests) requests.Add(requestLine + "\r\n" + headers + "\r\n" + new string(body, 0, read));
                if (ResponseDelayMs > 0) Thread.Sleep(ResponseDelayMs);
                var json = "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"" + Reply.Replace("\\", "\\\\").Replace("\"", "\\\"") + "\"}}]}";
                var bytes = Encoding.UTF8.GetBytes(json);
                var head = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: " + bytes.Length + "\r\nConnection: close\r\n\r\n");
                stream.Write(head, 0, head.Length);
                stream.Write(bytes, 0, bytes.Length);
                stream.Flush();
            }
        } catch { }
    }
    public void Dispose() { running = false; try { listener.Stop(); } catch { } }
}
'@

if ($ValidateOnly) {
    Write-Output '[OK] Conversation-panel harness compiled without launching the engine'
    return
}

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    Write-Output ('[OK] ' + $Message)
}

$project = Split-Path $PSScriptRoot -Parent
$run = Join-Path $project ('.artifacts/chat-panel-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff') + '-' + [Guid]::NewGuid().ToString('N'))
[void][System.IO.Directory]::CreateDirectory($run)
$appdata = Join-Path $run 'appdata'
$install = Join-Path $run 'install'
[void][System.IO.Directory]::CreateDirectory((Join-Path $appdata 'Delicium'))
[void][System.IO.Directory]::CreateDirectory($install)
$configPath = Join-Path $appdata 'Delicium/config.json'
$stderrPath = Join-Path $run 'stderr.log'

$fixture = [ChatFixture]::new()
$fixture.ResponseDelayMs = 1200
$port = $fixture.Port
$baseUrl = 'http://127.0.0.1:' + $port + '/v1'
$seed = [ordered]@{
    version = 1
    pixel_hittest = $true
    wander = $false
    language = 'en'
    default_model = 'bangboo-eous'
    chat = [ordered]@{
        enabled = $true
        base_url = $baseUrl
        model = 'fixture-model'
        api_key = 'sk-gui-fixture'
    }
    instances = @()
}
$seed | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $configPath -Encoding utf8

function Read-Config { Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json }
function Write-Config($Config) { $Config | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $configPath -Encoding utf8 }
function Wait-Log([string]$Pattern, [int]$TimeoutMs) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    do {
        if ((Get-Content -LiteralPath $stderrPath -Raw -ErrorAction SilentlyContinue) -match $Pattern) { return $true }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $deadline)
    return $false
}
function Get-FieldRow {
    $rect = [ChatPanelTest]::Client($controls)
    $scale = $rect.Width / 380.0
    $solution = $null
    foreach ($attempt in 1..5) {
        $shot = Join-Path $run ('scroll-' + $attempt + '.png')
        [ChatPanelTest]::Capture($controls, $shot)
        $bands = [ChatPanelTest]::Fields($shot)
        if ($bands.Count -gt 0) { $solution = $bands[$bands.Count - 1]; break }
        [ChatPanelTest]::Wheel(-120 * 12)
        Start-Sleep -Milliseconds 250
    }
    if ($null -eq $solution) { throw 'The conversation input field was not found in the control window' }
    return @{ X = $rect.Left + [int]$solution.X + [int]($solution.Width / 2)
              Y = $rect.Top + [int]$solution.Y + [int]($solution.Height / 2)
              Top = $rect.Top + [int]$solution.Y
              Height = $rect.Height; Width = $rect.Width; Scale = $scale
              LogicalX = ($solution.X + $solution.Width / 2) / $scale }
}
function Send-Chat([string]$Text, $Field) {
    Assert-Test ([ChatPanelTest]::Hit($Field.X, $Field.Y) -eq $controls) 'The conversation input is not covered by a foreign window'
    [ChatPanelTest]::Click($Field.X, $Field.Y)
    Start-Sleep -Milliseconds 150
    Set-Clipboard -Value $Text
    Start-Sleep -Milliseconds 150
    [ChatPanelTest]::SelectAll()
    [ChatPanelTest]::Paste()
    [ChatPanelTest]::Enter()
    Start-Sleep -Milliseconds 250
}

$oldAppData = $env:APPDATA
$oldRoot = $env:DELICIUM_INSTALL_ROOT
$oldNoConfig = $env:DELICIUM_NO_CONFIG
$process = $null
# PWsh's -eq on IntPtr compares object identity, not the value; compare the
# numeric handle explicitly. Without physical-pixel coordinates, ClientToScreen
# and CopyFromScreen disagree at non-1.0 scaling and capture the wrong area.
$dpiContext = [ChatPanelTest]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
if ([long]$dpiContext -eq 0) { throw 'Cannot enable physical-pixel DPI coordinates' }
try {
    # Reject an existing pet: the harness must only ever observe its own
    # windows, and a foreign instance would hold the same window titles.
    if (Get-Process -Name delicium -ErrorAction SilentlyContinue) { throw 'A Delicium engine is already running; close it before this test' }

    $env:APPDATA = $appdata
    $env:DELICIUM_INSTALL_ROOT = $install
    $env:DELICIUM_NO_CONFIG = '0'
    $process = Start-Process -FilePath $Executable -ArgumentList '--pet', 'bangboo-eous', '--no-wander', '--overlay-size', '960x640', '--exit-after', '180' -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError $stderrPath
    $deadline = [DateTime]::Now.AddSeconds(20)
    do {
        if ($process.HasExited) { throw "Delicium exited before its windows opened; see $run" }
        $scene = [ChatPanelTest]::Find($process.Id, 'Delicium Scene')
        $controls = [ChatPanelTest]::Find($process.Id, 'Delicium Controls')
        if ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($scene -ne [IntPtr]::Zero -and $controls -ne [IntPtr]::Zero) 'The pet scene and control window exist'

    [ChatPanelTest]::Raise($controls)
    Start-Sleep -Seconds 1
    $rectangle = [ChatPanelTest]::Client($controls)
    $scale = $rectangle.Width / 380.0

    # Click the heading (no widget there) to give the control window focus,
    # then park the cursor inside the scroll area and wheel to the bottom.
    [ChatPanelTest]::Click($rectangle.Left + [int](12 * $scale), $rectangle.Top + [int](10 * $scale))
    Start-Sleep -Milliseconds 250
    [void][ChatPanelTest]::SetCursorPos($rectangle.Left + [int]($rectangle.Width / 2), $rectangle.Top + [int]($rectangle.Height / 2))
    Start-Sleep -Milliseconds 100
    foreach ($step in 1..3) { [ChatPanelTest]::Wheel(-120 * 12); Start-Sleep -Milliseconds 200 }

    $field = Get-FieldRow
    $panel = Join-Path $run 'chat-panel.png'
    [ChatPanelTest]::Capture($controls, $panel)
    Write-Output ('capture=' + $panel)
    Assert-Test ($field.Y -gt $rectangle.Top -and $field.Y -lt $rectangle.Bottom) 'The conversation panel is visible after scrolling'
    Assert-Test ($fixture.Requests.Count -eq 0) 'No endpoint is contacted before the user sends anything'

    # ---- a remote turn: waiting state, then the fixture reply ----
    $before = Join-Path $run 'chat-before.png'
    [ChatPanelTest]::Capture($controls, $before)
    Send-Chat 'Hello from the GUI' $field
    $waiting = Join-Path $run 'chat-waiting.png'
    [ChatPanelTest]::Capture($controls, $waiting)
    $bandTop = [Math]::Max(0, $field.Top - [int](180 * $scale) - $rectangle.Top)
    $bandBottom = [Math]::Max(0, $field.Top - [int](8 * $scale) - $rectangle.Top)
    $typedChange = [ChatPanelTest]::Differences($before, $waiting, 0, $bandTop, $rectangle.Width, $bandBottom)
    Assert-Test ($typedChange -gt 40) ('The typed turn appears in the conversation (' + $typedChange + ' sampled pixels)')

    $deadline = [DateTime]::Now.AddSeconds(8)
    while ($fixture.Requests.Count -lt 1 -and [DateTime]::Now -lt $deadline) { Start-Sleep -Milliseconds 100 }
    $requests = $fixture.Requests
    Assert-Test ($requests.Count -eq 1) 'The fixture received exactly one request'
    $request = $requests[0]
    Assert-Test ($request.StartsWith('POST /v1/chat/completions ')) ('The request uses the completion route: ' + ($request.Split("`r")[0]))
    Assert-Test ($request -match '(?im)^Authorization: Bearer sk-gui-fixture\r?$') 'The API key travels in the Authorization header'
    Assert-Test (-not $request.Split("`r")[0].Contains('sk-gui-fixture')) 'The API key is absent from the request line'
    Assert-Test ($request -match '"model":"fixture-model"') 'The configured model name is sent'
    Assert-Test ($request -match '"content":"Hello from the GUI"') 'The typed message reaches the endpoint'

    # Wait for the reply to be polled into the history, then re-find the field
    # (the log grew, so the panel below it moved).
    Start-Sleep -Milliseconds 1000
    [ChatPanelTest]::Wheel(-120 * 3)
    Start-Sleep -Milliseconds 300
    $field = Get-FieldRow
    $replied = Join-Path $run 'chat-replied.png'
    [ChatPanelTest]::Capture($controls, $replied)
    Write-Output ('capture=' + $replied)
    $bandTop = [Math]::Max(0, $field.Top - [int](180 * $scale) - $rectangle.Top)
    $bandBottom = [Math]::Max(0, $field.Top - [int](8 * $scale) - $rectangle.Top)
    $replyChange = [ChatPanelTest]::Differences($waiting, $replied, 0, $bandTop, $rectangle.Width, $bandBottom)
    Assert-Test ($replyChange -gt 40) ('The remote reply appears in the conversation (' + $replyChange + ' sampled pixels)')

    # ---- live settings change: remote chat off, local lines ----
    $config = Read-Config
    $config.chat.enabled = $false
    Write-Config $config
    Assert-Test (Wait-Log 'applying the configured chat settings: enabled=false' 6000) 'Disabling remote chat is applied while the pet runs'
    $localBefore = Join-Path $run 'chat-local-before.png'
    [ChatPanelTest]::Capture($controls, $localBefore)
    $field = Get-FieldRow
    Send-Chat 'Are you local?' $field
    Start-Sleep -Milliseconds 700
    $localAfter = Join-Path $run 'chat-local-after.png'
    [ChatPanelTest]::Capture($controls, $localAfter)
    Write-Output ('capture=' + $localAfter)
    $bandTop = [Math]::Max(0, $field.Top - [int](180 * $scale) - $rectangle.Top)
    $bandBottom = [Math]::Max(0, $field.Top - [int](8 * $scale) - $rectangle.Top)
    $localChange = [ChatPanelTest]::Differences($localBefore, $localAfter, 0, $bandTop, $rectangle.Width, $bandBottom)
    Assert-Test ($localChange -gt 40) ('A local line appears without a remote request (' + $localChange + ' sampled pixels)')
    Assert-Test ($fixture.Requests.Count -eq 1) 'Disabling remote chat stops further endpoint contact'

    # Closing the scene ends the process; the control window would only hide
    # to the tray.
    [void][ChatPanelTest]::PostMessage($scene, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    if (-not $process.WaitForExit(15000)) { throw 'Delicium did not exit after the scene closed' }
    Assert-Test ($process.ExitCode -eq 0) ('The engine exits cleanly (code ' + $process.ExitCode + ')')
    Write-Output ('logs=' + $run)
    Write-Output '[OK] Conversation panel acceptance passed'
} finally {
    if ($process -ne $null -and -not $process.HasExited) {
        try { $process.Kill() } catch { }
        [void]$process.WaitForExit(5000)
    }
    $fixture.Dispose()
    $env:APPDATA = $oldAppData
    if ($null -eq $oldRoot) { Remove-Item Env:DELICIUM_INSTALL_ROOT -ErrorAction SilentlyContinue }
    else { $env:DELICIUM_INSTALL_ROOT = $oldRoot }
    if ($null -eq $oldNoConfig) { Remove-Item Env:DELICIUM_NO_CONFIG -ErrorAction SilentlyContinue }
    else { $env:DELICIUM_NO_CONFIG = $oldNoConfig }
}
