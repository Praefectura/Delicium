param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/release/delicium-launcher.exe'),
    [switch]$ValidateOnly
)

# Settings-page acceptance: launches the launcher's native settings page with a
# temporary APPDATA and installation root, drives the real controls through
# WM_COMMAND, and captures both languages. The user's own configuration is
# never read or written.
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
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public static class SettingsPageTest {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    public delegate bool Enumerate(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")] public static extern bool EnumWindows(Enumerate callback, IntPtr parameter);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, System.Text.StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr window, out Rect rectangle);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr window, int id);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll", EntryPoint="SendMessageW", CharSet=CharSet.Unicode)] public static extern IntPtr SendText(IntPtr window, uint message, IntPtr w, string text);
    [DllImport("user32.dll", EntryPoint="SendMessageW")] public static extern IntPtr Send(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtr(IntPtr window, int index);

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
        if (!GetClientRect(window, out rectangle) || !ClientToScreen(window, ref origin)) throw new Exception("Cannot inspect the settings window");
        return new Rectangle(origin.X, origin.Y, rectangle.Right, rectangle.Bottom);
    }
    public static void Raise(IntPtr window) {
        // The page is a normal (non-topmost) window and the desktop under a
        // test run can hold a maximized browser or a terminal. Move it into
        // the topmost band for the capture, then back out, so CopyFromScreen
        // reads the page and not whatever was raised over it.
        if (!SetWindowPos(window, new IntPtr(-1), 0, 0, 0, 0, 0x0013 | 0x0040)) throw new Exception("Cannot raise the settings window");
        SetForegroundWindow(window);
        System.Threading.Thread.Sleep(400);
    }
    public static void Lower(IntPtr window) {
        if (!SetWindowPos(window, new IntPtr(-2), 0, 0, 0, 0, 0x0013 | 0x0040)) throw new Exception("Cannot lower the settings window");
        System.Threading.Thread.Sleep(200);
    }
    public static void Capture(IntPtr window, string path) {
        var rectangle = Client(window);
        var center = new Point { X = rectangle.Left + rectangle.Width / 2, Y = rectangle.Top + rectangle.Height / 2 };
        if (GetAncestor(WindowFromPoint(center), 2) != window) throw new Exception("Capture aborted: the settings window is covered");
        using (var bitmap = new Bitmap(rectangle.Width, rectangle.Height)) {
            using (var graphics = Graphics.FromImage(bitmap)) graphics.CopyFromScreen(rectangle.Location, System.Drawing.Point.Empty, rectangle.Size);
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
    public static int DistinctColors(string path) {
        var seen = new System.Collections.Generic.HashSet<int>();
        using (var bitmap = new Bitmap(path)) {
            for (int y = 0; y < bitmap.Height; y += 3) for (int x = 0; x < bitmap.Width; x += 3)
                seen.Add(bitmap.GetPixel(x, y).ToArgb());
        }
        return seen.Count;
    }
    public static int Differences(string first, string second) {
        using (var a = new Bitmap(first)) using (var b = new Bitmap(second)) {
            if (a.Width != b.Width || a.Height != b.Height) throw new Exception("Capture sizes differ");
            int count = 0;
            for (int y = 0; y < a.Height; y += 2) for (int x = 0; x < a.Width; x += 2) {
                var left = a.GetPixel(x, y); var right = b.GetPixel(x, y);
                if (Math.Abs(left.R-right.R) > 8 || Math.Abs(left.G-right.G) > 8 || Math.Abs(left.B-right.B) > 8) count++;
            }
            return count;
        }
    }
}
'@

if ($ValidateOnly) {
    Write-Output '[OK] Settings-page harness compiled without launching the launcher'
    return
}

function Assert-Test([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    Write-Output ('[OK] ' + $Message)
}

$project = Split-Path $PSScriptRoot -Parent
$run = Join-Path $project ('.artifacts/settings-page-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff') + '-' + [Guid]::NewGuid().ToString('N'))
[void][System.IO.Directory]::CreateDirectory($run)
$appdata = Join-Path $run 'appdata'
$install = Join-Path $run 'install'
[void][System.IO.Directory]::CreateDirectory((Join-Path $appdata 'Delicium'))
[void][System.IO.Directory]::CreateDirectory($install)
$configPath = Join-Path $appdata 'Delicium/config.json'
$seed = [ordered]@{
    version = 1
    pixel_hittest = $true
    wander = $true
    language = 'en'
    default_model = 'bangboo-eous'
    future_keep = 7
    chat = [ordered]@{
        enabled = $false
        base_url = ''
        model = ''
        api_key = ''
        later = 'keep'
    }
    instances = @()
}
$seed | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $configPath -Encoding utf8

function Read-Config {
    Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
}
function Wait-Config([scriptblock]$Predicate, [string]$Message) {
    $deadline = [DateTime]::Now.AddSeconds(5)
    do {
        $config = Read-Config
        if (& $Predicate $config) { return $config }
        Start-Sleep -Milliseconds 60
    } while ([DateTime]::Now -lt $deadline)
    throw ('Timed out waiting for configuration: ' + $Message)
}

# WM_* constants used by the page.
$WM_SETTEXT = 0x000C
$WM_COMMAND = 0x0111
$WM_CLOSE = 0x0010
$BM_CLICK = 0x00F5
$CB_SETCURSEL = 0x014E
$BN_CLICKED = 0
$CBN_SELCHANGE = 1

# Control ids mirror src/launcher/settings.rs.
$ID_HITTEST = 1001
$ID_WANDER = 1015
$ID_LANGUAGE = 1017
$ID_CHAT_ENABLED = 1022
$ID_CHAT_URL = 1024
$ID_CHAT_MODEL = 1026
$ID_CHAT_KEY = 1028
$ID_CHAT_SAVE = 1029
$ID_CHAT_STATUS = 1030

# PWsh's -eq on IntPtr compares object identity, not the value; compare the
# numeric handle explicitly.
$dpiContext = [SettingsPageTest]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
if ([long]$dpiContext -eq 0) { throw 'Cannot enable physical-pixel DPI coordinates' }
$oldAppData = $env:APPDATA
$oldRoot = $env:DELICIUM_INSTALL_ROOT
$process = $null
$window = [IntPtr]::Zero
try {
    $env:APPDATA = $appdata
    $env:DELICIUM_INSTALL_ROOT = $install
    $process = Start-Process -FilePath $Executable -ArgumentList '--settings', '--no-update' -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $run 'stdout.log') -RedirectStandardError (Join-Path $run 'stderr.log')
    $deadline = [DateTime]::Now.AddSeconds(15)
    do {
        if ($process.HasExited) { throw "The launcher exited before the page opened; see $run" }
        $window = [SettingsPageTest]::Find($process.Id, 'Delicium Settings')
        if ($window -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($window -ne [IntPtr]::Zero) 'The settings page opens with the configured English title'

    # The page is a normal window, not an always-on-top one: its extended
    # style must never carry WS_EX_TOPMOST (a capture helper may raise it
    # temporarily, but the page itself must not stay in that band).
    $extended = [SettingsPageTest]::GetWindowLongPtr($window, -20).ToInt64()
    Assert-Test (($extended -band 0x8) -eq 0) ('The page is not an always-on-top window (ex-style 0x' + $extended.ToString('X') + ')')

    [SettingsPageTest]::Raise($window)
    $english = Join-Path $run 'settings-en.png'
    [SettingsPageTest]::Capture($window, $english)
    $colors = [SettingsPageTest]::DistinctColors($english)
    Assert-Test ($colors -gt 30) ('The English page renders controls and text (' + $colors + ' distinct colours)')
    Write-Output ('capture=' + $english)

    # Both checkboxes write through the same path the engine reads.
    $hitTest = [SettingsPageTest]::GetDlgItem($window, $ID_HITTEST)
    Assert-Test ($hitTest -ne [IntPtr]::Zero) 'The hit-region checkbox exists'
    [void][SettingsPageTest]::Send($hitTest, $BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero)
    $config = Wait-Config { param($c) $c.pixel_hittest -eq $false } 'pixel_hittest becomes false'
    Assert-Test ($config.future_keep -eq 7) 'An unknown top-level field survives a page write'

    $wander = [SettingsPageTest]::GetDlgItem($window, $ID_WANDER)
    Assert-Test ($wander -ne [IntPtr]::Zero) 'The wander checkbox exists'
    [void][SettingsPageTest]::Send($wander, $BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero)
    $config = Wait-Config { param($c) $c.wander -eq $false } 'wander becomes false'
    Assert-Test ($config.chat.later -eq 'keep') 'An unknown field inside chat survives checkbox writes'

    # Language combo: select the second item and report the change exactly as
    # the combo box does.
    $combo = [SettingsPageTest]::GetDlgItem($window, $ID_LANGUAGE)
    Assert-Test ($combo -ne [IntPtr]::Zero) 'The language combo exists'
    [void][SettingsPageTest]::Send($combo, $CB_SETCURSEL, [IntPtr]::new(1), [IntPtr]::Zero)
    $command = [IntPtr]::new((($CBN_SELCHANGE -shl 16) -bor $ID_LANGUAGE))
    [void][SettingsPageTest]::PostMessage($window, $WM_COMMAND, $command, $combo)
    $config = Wait-Config { param($c) $c.language -eq 'zh_hans' } 'language becomes zh_hans'
    $deadline = [DateTime]::Now.AddSeconds(5)
    do {
        $title = [SettingsPageTest]::Text($window)
        if ($title -eq 'Delicium 设置') { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($title -eq 'Delicium 设置') 'The page title re-localizes to Simplified Chinese'

    [SettingsPageTest]::Raise($window)
    $chinese = Join-Path $run 'settings-zh.png'
    [SettingsPageTest]::Capture($window, $chinese)
    $changed = [SettingsPageTest]::Differences($english, $chinese)
    Assert-Test ($changed -gt 500) ('The Chinese page differs from the English one (' + $changed + ' sampled pixels)')
    Write-Output ('capture=' + $chinese)

    # An insecure remote URL is refused and the checkbox goes back.
    $url = [SettingsPageTest]::GetDlgItem($window, $ID_CHAT_URL)
    $model = [SettingsPageTest]::GetDlgItem($window, $ID_CHAT_MODEL)
    $enabled = [SettingsPageTest]::GetDlgItem($window, $ID_CHAT_ENABLED)
    $status = [SettingsPageTest]::GetDlgItem($window, $ID_CHAT_STATUS)
    Assert-Test (($url -ne [IntPtr]::Zero) -and ($model -ne [IntPtr]::Zero) -and ($enabled -ne [IntPtr]::Zero)) 'The chat controls exist'
    [void][SettingsPageTest]::SendText($url, $WM_SETTEXT, [IntPtr]::Zero, 'http://example.com/v1')
    [void][SettingsPageTest]::SendText($model, $WM_SETTEXT, [IntPtr]::Zero, 'test-model')
    [void][SettingsPageTest]::Send($enabled, $BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 300
    $message = [SettingsPageTest]::Text($status)
    Assert-Test ($message -match 'https') ('An insecure remote URL is refused: ' + $message)
    $config = Read-Config
    Assert-Test ($config.chat.enabled -eq $false) 'A refused URL does not enable remote conversations'

    # A valid https endpoint saves the whole group.
    [void][SettingsPageTest]::SendText($url, $WM_SETTEXT, [IntPtr]::Zero, 'https://api.example.com/v1')
    [void][SettingsPageTest]::Send($enabled, $BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero)
    $config = Wait-Config { param($c) $c.chat.enabled -eq $true } 'chat becomes enabled'
    Assert-Test ($config.chat.base_url -eq 'https://api.example.com/v1') 'The base URL is stored'
    Assert-Test ($config.chat.model -eq 'test-model') 'The model name is stored'

    $key = [SettingsPageTest]::GetDlgItem($window, $ID_CHAT_KEY)
    Assert-Test ($key -ne [IntPtr]::Zero) 'The API key field exists'
    [void][SettingsPageTest]::SendText($key, $WM_SETTEXT, [IntPtr]::Zero, 'sk-test-not-a-real-key')
    $save = [SettingsPageTest]::GetDlgItem($window, $ID_CHAT_SAVE)
    [void][SettingsPageTest]::Send($save, $BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero)
    $config = Wait-Config { param($c) $c.chat.api_key -eq 'sk-test-not-a-real-key' } 'the API key is stored'
    Assert-Test ($config.chat.later -eq 'keep') 'The merged chat group keeps unknown fields'
    Assert-Test ($config.instances.Count -eq 0) 'The saved instances survive untouched'
    $saved = [SettingsPageTest]::Text($status)
    Assert-Test ($saved -eq '已保存。') ('Saving reports success in the page language: ' + $saved)

    [SettingsPageTest]::Raise($window)
    $chat = Join-Path $run 'settings-chat.png'
    [SettingsPageTest]::Capture($window, $chat)
    Write-Output ('capture=' + $chat)

    # WM_CLOSE in --settings mode destroys the page and ends the launcher.
    [void][SettingsPageTest]::PostMessage($window, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
    if (-not $process.WaitForExit(10000)) { throw 'The launcher did not exit after the page closed' }
    Assert-Test ($process.ExitCode -eq 0) ('The launcher exits cleanly (code ' + $process.ExitCode + ')')
    Write-Output ('logs=' + $run)
    Write-Output '[OK] Settings page acceptance passed'
} finally {
    if ($process -ne $null -and -not $process.HasExited) {
        try { $process.Kill() } catch { }
        [void]$process.WaitForExit(5000)
    }
    $env:APPDATA = $oldAppData
    if ($null -eq $oldRoot) { Remove-Item Env:DELICIUM_INSTALL_ROOT -ErrorAction SilentlyContinue }
    else { $env:DELICIUM_INSTALL_ROOT = $oldRoot }
}
