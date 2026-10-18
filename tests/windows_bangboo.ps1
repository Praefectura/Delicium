param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/debug/delicium.exe'),
    [switch]$ValidateOnly
)

# Stable two-pet acceptance: --no-wander pins positions, not social playback.
# Reuse only the trusted actions harness's C# and named helper definitions;
# never dot-source its top-level launch/input code. All input/capture guards
# therefore stay identical to the existing independently checked helpers.
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
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'windows_actions.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'Cannot parse shared actions harness' }
$csharp = @($ast.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.StringConstantExpressionAst] -and
    $node.Value.StartsWith('using System;') -and $node.Value.Contains('public static class ActionsNative')
}, $true))
if ($csharp.Count -ne 1) { throw 'Shared native helper must have exactly one definition' }
Add-Type -ReferencedAssemblies $references -TypeDefinition $csharp[0].Value
$names = @('Assert-Test', 'Get-LogLines', 'Count-Log', 'Wait-LogCount', 'Wait-Log',
    'Screen-Point', 'Ensure-InputTarget', 'Ensure-InputGuard', 'Capture-Scene')
foreach ($name in $names) {
    $definitions = @($ast.FindAll({ param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $true))
    if ($definitions.Count -ne 1) { throw ('Ambiguous shared helper: ' + $name) }
    . ([scriptblock]::Create($definitions[0].Extent.Text))
}
if ($ValidateOnly) {
    Write-Output '[OK] Bangboo harness and shared guarded helpers compiled without launching windows'
    return
}

$invariant = [System.Globalization.CultureInfo]::InvariantCulture
$project = Split-Path $PSScriptRoot -Parent
$script:run = Join-Path $project ('.artifacts/windows-bangboo-' + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff') + '-' + [Guid]::NewGuid().ToString('N'))
[void][System.IO.Directory]::CreateDirectory($script:run)
$script:logPath = Join-Path $script:run 'stderr.log'
$script:process = $null
$script:scene = [IntPtr]::Zero
$script:controls = [IntPtr]::Zero
$script:backdrop = [IntPtr]::Zero
$script:rectangle = $null
$script:span = 0.0; $script:bottom = 0.0
$script:captures = @(); $script:mouseDown = $false
$oldConfig = $env:DELICIUM_NO_CONFIG

function Wait-Fresh([int]$From, [string]$Pattern, [int]$TimeoutMs = 5000) {
    $deadline = [DateTime]::Now.AddMilliseconds($TimeoutMs)
    do {
        $lines = Get-LogLines
        for ($i = $From; $i -lt $lines.Count; $i++) {
            if ([string]$lines[$i] -match $Pattern) { return [string]$lines[$i] }
        }
        Start-Sleep -Milliseconds 30
    } while ([DateTime]::Now -lt $deadline)
    throw ('Timed out waiting for fresh event: ' + $Pattern)
}
function Assert-PairAction([string]$Action) {
    $lines = Get-LogLines
    foreach ($id in 1,2) {
        $latest = @($lines | Select-String ('action change instance='+$id+' action=')) | Select-Object -Last 1
        if ([string]$latest -notmatch ('action change instance='+$id+' action='+$Action+' restart=')) {
            throw ('Capture phase changed for instance '+$id+': expected '+$Action)
        }
    }
}
function Anchor([double]$WorldX) {
    foreach ($height in @(0.7, 0.6, 0.8, 0.5)) {
        $point = Screen-Point $WorldX $height
        $x = $script:rectangle.Left + $point[0]; $y = $script:rectangle.Top + $point[1]
        if ([ActionsNative]::Hit($x,$y) -eq $script:scene) { return [int[]]@($x,$y) }
    }
    throw 'Cannot find a guarded point on the expected Bangboo body'
}
function Press([int[]]$Point) {
    Ensure-InputTarget $Point[0] $Point[1]
    if ([ActionsNative]::LeftButtonDown()) { throw 'Physical input guard: left button already held' }
    if (-not [ActionsNative]::SetCursorPos($Point[0],$Point[1])) { throw 'Cannot position cursor' }
    Start-Sleep -Milliseconds 40
    Ensure-InputGuard $Point[0] $Point[1]
    $script:mouseDown = $true
    [ActionsNative]::Mouse(2)
}
function Release {
    [ActionsNative]::Mouse(4)
    $script:mouseDown = $false
}
function Shot([string]$Name) {
    $path = Capture-Scene $Name
    Assert-Test ([ActionsNative]::Samples($path) -gt 150) ('Visible Bangboo capture: ' + $path) | Out-Host
    return $path
}
function Click-Control([double]$LogicalX, [double]$LogicalY) {
    $rect = [ActionsNative]::Client($script:controls)
    $scale = $rect.Width / 380.0
    $x = $rect.Left + [int][Math]::Round($LogicalX * $scale)
    $y = $rect.Top + [int][Math]::Round($LogicalY * $scale)
    $guard = {
        if ($script:process.HasExited -or
            [ActionsNative]::Owner($script:controls) -ne $script:process.Id -or
            [ActionsNative]::Hit($x,$y) -ne $script:controls) {
            throw 'Physical input guard: control target is not owned and uncovered'
        }
    }
    & $guard
    if ([ActionsNative]::LeftButtonDown()) { throw 'Physical input guard: left button already held' }
    if (-not [ActionsNative]::SetCursorPos($x,$y)) { throw 'Cannot position control cursor' }
    Start-Sleep -Milliseconds 40
    & $guard
    $actual = [ActionsNative+Point]::new()
    if (-not [ActionsNative]::GetCursorPos([ref]$actual) -or $actual.X -ne $x -or $actual.Y -ne $y) {
        throw 'Physical input guard: control cursor moved before press'
    }
    $script:mouseDown = $true
    [ActionsNative]::Mouse(2)
    Start-Sleep -Milliseconds 80
    Release
}

if ([ActionsNative]::SetThreadDpiAwarenessContext([IntPtr]::new(-4)) -eq [IntPtr]::Zero) { throw 'Cannot enable physical-pixel coordinates' }
$cursor = [ActionsNative+Point]::new()
if (-not [ActionsNative]::GetCursorPos([ref]$cursor)) { throw 'Cannot save cursor' }
$passed = $false
try {
    if (-not (Test-Path -LiteralPath $Executable)) { throw 'Executable does not exist' }
    $env:DELICIUM_NO_CONFIG = '1'
    $script:process = Start-Process -FilePath $Executable -ArgumentList '--bangboo','--no-wander','--overlay-size','960x640','--exit-after','120' -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $script:run 'stdout.log') -RedirectStandardError $script:logPath
    $deadline = [DateTime]::Now.AddSeconds(15)
    do {
        if ($script:process.HasExited) { throw 'Application exited before readiness' }
        $script:scene = [ActionsNative]::Find($script:process.Id,'Delicium Scene')
        $script:controls = [ActionsNative]::Find($script:process.Id,'Delicium Controls')
        if ($script:scene -ne [IntPtr]::Zero -and $script:controls -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::Now -lt $deadline)
    Assert-Test ($script:scene -ne [IntPtr]::Zero -and $script:controls -ne [IntPtr]::Zero) 'PID-scoped scene and controls exist'
    Assert-Test (([ActionsNative]::Owner($script:scene) -eq $script:process.Id) -and ([ActionsNative]::Owner($script:controls) -eq $script:process.Id)) 'Both handles belong to the launched process'
    Wait-Log 'spawn instance=\d+ asset=\d+ kind=skeleton3d' 2 15000
    Assert-Test ((Count-Log 'spawn instance=') -eq 2) 'Bangboo mode spawns exactly two 3D pets'
    $initialAsset = [regex]::Match([string](@(Get-LogLines | Select-String 'spawn instance=1 ') | Select-Object -Last 1), 'asset=(\d+)')
    Assert-Test $initialAsset.Success 'The initial bundled Bangboo asset is identifiable'
    Wait-Log 'social pair first=1 second=2 phase=greet' 1 10000
    Wait-Log 'hit_region mode=pixel' 1 10000
    $script:rectangle = [ActionsNative]::Client($script:scene)
    Assert-Test (-not [ActionsNative]::SpansPrimaryMonitor($script:rectangle)) 'Capture is bounded and cannot cover the desktop'
    $view = [regex]::Match([string](@(Get-LogLines | Select-String 'scene_view ') | Select-Object -Last 1), 'scene_view mode=bounded span=([0-9.]+) bottom=(-?[0-9.]+)')
    Assert-Test $view.Success 'Bounded scene mapping is parseable'
    $script:span = [double]::Parse($view.Groups[1].Value,$invariant)
    $script:bottom = [double]::Parse($view.Groups[2].Value,$invariant)
    $script:backdrop = [ActionsBackdrop]::Open($script:rectangle)
    [ActionsNative]::Raise($script:scene); [ActionsNative]::Raise($script:controls)
    [ActionsBackdrop]::AnchorBelow($script:scene)
    [ActionsNative]::Raise($script:scene); [ActionsNative]::Raise($script:controls)
    Start-Sleep -Milliseconds 150
    Assert-Test ([ActionsNative]::Hit($script:rectangle.Left+40,$script:rectangle.Top+40) -eq $script:backdrop) 'Empty scene passes through to the owned backdrop'

    # Setup can outlast the first greeting. Synchronize on a new cycle rather
    # than treating any historical startup event as the current capture phase.
    $captureFrom = (Get-LogLines).Count
    [void](Wait-Fresh $captureFrom 'social pair first=1 second=2 phase=greet' 20000)
    $playFrom = (Get-LogLines).Count
    foreach ($id in 1,2) { [void](Wait-Fresh $captureFrom ('action change instance='+$id+' action=greet restart=false') 1500) }
    # Capture the raised-hand wave, not its neutral first/transition frame.
    Start-Sleep -Milliseconds 350
    Assert-PairAction 'greet'
    $greet = Shot 'scene-greet.png'
    Assert-PairAction 'greet'
    Start-Sleep -Milliseconds 300
    Assert-PairAction 'greet'
    $greetLater = Shot 'scene-greet-later.png'
    Assert-PairAction 'greet'
    Assert-Test ([ActionsNative]::Difference($greet,$greetLater) -gt 40) 'Two greeting phases visibly show the wave changing'
    [void](Wait-Fresh $playFrom 'social pair first=1 second=2 phase=play' 5000)
    $endFrom = (Get-LogLines).Count
    foreach ($id in 1,2) { [void](Wait-Fresh $playFrom ('action change instance='+$id+' action=play restart=false') 1500) }
    Start-Sleep -Milliseconds 250
    Assert-PairAction 'play'
    $playA = Shot 'scene-play-a.png'
    Assert-PairAction 'play'
    Start-Sleep -Milliseconds 450
    Assert-PairAction 'play'
    $playB = Shot 'scene-play-b.png'
    Assert-PairAction 'play'
    Assert-Test ([ActionsNative]::Difference($greet,$playA) -gt 40) 'Greeting and play have distinct rendered poses'
    Assert-Test ([ActionsNative]::Difference($playA,$playB) -gt 40) 'Two play phases visibly differ while both world positions are pinned'
    [void](Wait-Fresh $endFrom 'social pair first=1 second=2 phase=end' 5000)
    Start-Sleep -Milliseconds 250
    $idle = Shot 'scene-idle.png'

    # No overlapping silhouettes: the two model centers are 1.3 world units apart.
    $point = Anchor -0.65
    $from = (Get-LogLines).Count
    Press $point
    Start-Sleep -Milliseconds 80
    Release
    [void](Wait-Fresh $from 'drag start instance=1 ')
    [void](Wait-Fresh $from 'poke instance=1 ')
    [void](Wait-Fresh $from 'action change instance=1 action=reaction restart=false')
    Start-Sleep -Milliseconds 150
    $reaction = Shot 'scene-reaction.png'
    Assert-Test ([ActionsNative]::Difference($idle,$reaction) -gt 40) 'Physical 3D click produces a visibly different reaction'
    $point = Anchor -0.65
    $repeatFrom = (Get-LogLines).Count
    Press $point
    Start-Sleep -Milliseconds 60
    Release
    [void](Wait-Fresh $repeatFrom 'poke instance=1 ')
    [void](Wait-Fresh $repeatFrom 'action change instance=1 action=reaction restart=true')
    Assert-Test $true 'A real repeat click restarts the 3D reaction inside its active window'
    [void](Wait-Fresh $repeatFrom 'action change instance=1 action=idle restart=false' 3000)

    # Wait for a new pair, then interrupt it with an actual held/moved drag.
    $pairFrom = (Get-LogLines).Count
    [void](Wait-Fresh $pairFrom 'social pair first=1 second=2 phase=greet' 18000)
    $point = Anchor -0.65
    $dragFrom = (Get-LogLines).Count
    $pokesBefore = Count-Log 'poke instance='
    Press $point
    [void](Wait-Fresh $dragFrom 'drag start instance=1 ')
    [void](Wait-Fresh $dragFrom 'social pair first=1 second=2 phase=cancel')
    foreach ($step in 1..6) {
        if (-not [ActionsNative]::SetCursorPos($point[0]+8*$step,$point[1])) { throw 'Cannot move drag cursor' }
        Start-Sleep -Milliseconds 60
        Ensure-InputGuard ($point[0]+8*$step) $point[1]
    }
    Start-Sleep -Milliseconds 1100
    Ensure-InputGuard ($point[0]+48) $point[1]
    Assert-Test ([ActionsNative]::LeftButtonDown()) 'Drag button remained held for the full pause'
    $holdLines = @((Get-LogLines) | Select-Object -Skip $dragFrom)
    Assert-Test (@($holdLines | Select-String 'action change instance=[12] action=(greet|play|walk)').Count -eq 0) 'Both pets leave the social action and neither walks during the held drag'
    $releaseFrom = (Get-LogLines).Count
    Release
    $released = Wait-Fresh $releaseFrom 'drag release instance=1 x=(-?[0-9.]+)'
    $releaseX = [double]::Parse(([regex]::Match($released,'x=(-?[0-9.]+)')).Groups[1].Value,$invariant)
    Assert-Test ($releaseX -gt -0.55 -and $releaseX -lt -0.2) 'Drag changes the actual pet world position by the expected small distance'
    Assert-Test ((Count-Log 'poke instance=') -eq $pokesBefore) 'A real drag does not accidentally trigger a click'
    [void](Shot 'scene-after-drag.png')
    [void](Wait-Fresh $releaseFrom 'social pair first=1 second=2 phase=greet' 18000)
    Assert-Test $true 'Both pets can pair again after drag release and cooldown'

    # Exercise the real controls: another 3D pet must reuse the bundled asset,
    # while Add 2D must retain the existing textured 2D source.
    $addFrom = (Get-LogLines).Count
    Click-Control 91 101
    $added3d = Wait-Fresh $addFrom 'spawn instance=3 asset=(\d+) kind=skeleton3d' 5000
    $asset3d = [regex]::Match($added3d, 'asset=(\d+)')
    Assert-Test ($asset3d.Groups[1].Value -eq $initialAsset.Groups[1].Value) 'Add 3D adds another Bangboo from the same bundled asset'
    $addFrom = (Get-LogLines).Count
    Click-Control 34 101
    $added2d = Wait-Fresh $addFrom 'spawn instance=4 asset=(\d+) kind=sprite2d' 10000
    $asset2d = [regex]::Match($added2d, 'asset=(\d+)')
    Assert-Test ($asset2d.Groups[1].Value -ne $initialAsset.Groups[1].Value) 'Add 2D retains the separate textured 2D asset'
    Assert-Test ((Count-Log 'spawn instance=') -eq 4) 'Both physical Add buttons each add exactly one character'
    Assert-Test ((Count-Log 'hit_region mode=fallback') -eq 0) 'No native hit-region fallback occurred'
    Assert-Test ([ActionsNative]::HasRegion($script:scene)) 'Native character hit regions remain installed'
    $passed = $true
} finally {
    try {
        if ($script:mouseDown) { Release }
        [ActionsBackdrop]::Shut()
        if ($script:process -and -not $script:process.HasExited) {
            if ($script:scene -ne [IntPtr]::Zero -and [ActionsNative]::Owner($script:scene) -eq $script:process.Id) {
                [void][ActionsNative]::PostMessage($script:scene,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)
            }
            if (-not $script:process.WaitForExit(1500) -and -not $script:process.HasExited) {
                $script:process.Kill(); [void]$script:process.WaitForExit(1500)
            }
        }
    } finally {
        [void][ActionsNative]::SetCursorPos($cursor.X,$cursor.Y)
        $env:DELICIUM_NO_CONFIG = $oldConfig
    }
}
if ($passed) {
    Assert-Test ($script:process.HasExited -and $script:process.ExitCode -eq 0) 'Test-owned app exited successfully'
    foreach ($path in $script:captures) { Write-Output ('screenshot='+$path) }
    Write-Output ('WINDOWS_RESULT pass=true artifacts='+$script:run)
}
