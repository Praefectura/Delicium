param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/release/delicium.exe'),
    [switch]$Quick,
    [switch]$Wander,
    [switch]$ValidateOnly
)

# Local release-baseline benchmark harness for Delicium.
#
# Runs the application's own --bench workload in fresh processes, samples the
# held child process at about 1 Hz, validates the BENCH_EVENT/BENCH_RESULT
# stdout protocol, and writes per-case artifacts plus a run summary.
#
# Boundaries:
#  - Only the child started below is queried or stopped. Unrelated processes
#    and windows are never touched; there is no name or wildcard based kill.
#  - If any preexisting Delicium process is found the run aborts before the
#    first case. Do not launch the application while this script is running.
#  - No input injection, no screenshots, no network, no user files.
#  - Output goes only to a new .artifacts/windows-bench-<stamp> subtree;
#    existing artifacts are never overwritten or cleaned. -Wander runs the
#    same case matrix with --bench-wander under a windows-bench-wander-<stamp>
#    subtree; the terminal config.wander flag is checked against the plan.
#  - -ValidateOnly compiles the native binding and self-checks the protocol
#    and resource arithmetic in memory; it starts no GUI, starts no child
#    process, and writes nothing.
#
# Child stdout protocol (v1):
#   BENCH_EVENT {"event":"phase"|"snapshot"|"reload_complete"|"failure",...}
#   BENCH_RESULT {"schema_version":1,...}      exactly one terminal line
#
# Measurement caveats kept in every report:
#  - Process CPU is core-seconds; stage timings are host-side wall-clock costs.
#  - submitted_per_second counts scene submit/present requests, not the
#    display refresh rate.
#  - GPU execution time and VRAM usage are not measured (gpu_execution_ms
#    is null); process memory is not VRAM.
#  - Allocator, driver and grow-only buffer capacities need not shrink; an
#    increase between windows is not evidence of a leak.

$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent

$script:ScriptVersion = 3
$script:GuiReadyTimeoutSeconds = 30
$script:WarmupSeconds = 10
$script:ReloadBaselineSeconds = 5
$script:SettleSeconds = 10
$script:MinSecondsPerSuccess = 0.25
$script:TimeEpsilon = 0.05
$script:DefaultBenchSeconds = 30
$script:DefaultBenchReloads = 100
$script:DefaultExitAfter = 600
$script:StartupAllowanceSeconds = 90
$script:PostRunSlackSeconds = 30
$script:LogicalCores = [System.Environment]::ProcessorCount

$script:StageNames = @(
    'advance',
    'render_frame',
    'hit_geometry',
    'hit_compare',
    'native_region_build_install',
    'acquire',
    'encode_upload',
    'submit',
    'present',
    'scene_callback',
    'control_callback',
    'frame_interval'
)

$script:HarnessAssumptions = @(
    'Terminal config.build_profile must be "release"; the executable path is release by default and production builds emit release executables.',
    'Terminal config.wander is required and must match the case plan (false for the stationary default workload, true only under -Wander); it is the protocol provenance for the wandering workload because the child stderr log level stays at warn.',
    'Stage timing keys are present as null when their count is 0; a positive count must carry finite nonnegative timings.',
    'Snapshots are expected to be emitted only after setup (warmup, measure, reload baseline/cycle, settle), so every snapshot must show instance_count = N and cpu_assets = gpu_assets = expected_assets.',
    'The expected asset count is a derived set (live instance references union the latest examples cache); for the reload layout it may legitimately climb 2 -> 3 -> 4 and is not required to be 3. Only snapshot totals are checked: instance_count and cpu/gpu/expected assets, never a fixed sprite/skeleton split.',
    'Resource sampling begins only after the held child has a visible main window, with a 30-second readiness timeout. No GUI-resource query is attempted before readiness. After readiness, a query failure remains null plus its error code and fails the case; zero with no error remains valid.',
    'measured_seconds is the actual sum of measured phase windows: measure for timed cases, or reload_baseline + reload + settle for reload cases. Timed windows must meet the planned minimum; event-loop deadline overshoot is recorded rather than rejected.',
    'Timed cases require phases "warmup" and "measure"; the reload case requires "warmup", "reload_baseline", "reload" and "settle".',
    'Reload accounting requires requested = successful = render_confirmed = expected = 100; request ids are opaque unique app ids that include the two setup loads, the cycle field carries the 1..100 completion cycle, and the final completion may legitimately be emitted after the transition into settle.',
    'The harness never derives counts from planned values; all counts are read from the child terminal result and events.',
    'Actual requested/logical/physical sizes, both window scale factors, adapter/backend/device type, present/alpha mode and format are required configuration facts; no host DPI scale is assumed.',
    'A Delicium process appearing during a case invalidates and stops the benchmark; foreign processes are never touched and remaining cases are not launched.'
)

# ---------------------------------------------------------------------------
# Native binding: GetGuiResources plus process image query for identity checks
# ---------------------------------------------------------------------------

if (-not ('BenchNative' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class BenchNative {
    public const uint GdiObjects = 0;
    public const uint UserObjects = 1;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetGuiResources(IntPtr process, uint flags);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder name, ref uint size);

    private static void ClearLastError() {
        try { Marshal.SetLastPInvokeError(0); } catch { }
    }

    private static int[] Read(IntPtr process) {
        ClearLastError();
        uint gdi = GetGuiResources(process, GdiObjects);
        int gdiError = (gdi == 0) ? Marshal.GetLastWin32Error() : 0;
        ClearLastError();
        uint user = GetGuiResources(process, UserObjects);
        int userError = (user == 0) ? Marshal.GetLastWin32Error() : 0;
        int[] result = new int[4];
        result[0] = unchecked((int)gdi);
        result[1] = gdiError;
        result[2] = unchecked((int)user);
        result[3] = userError;
        return result;
    }

    // GetGuiResources can report a stale failure on a handle it can actually
    // query; retry the pair once and keep the cleaner reading. Errors are
    // preserved when the query really fails so callers never see a fake 0.
    public static int[] Gui(IntPtr process) {
        int[] first = Read(process);
        if (first[1] == 0 && first[3] == 0) { return first; }
        int[] second = Read(process);
        int firstErrors = (first[1] != 0 ? 1 : 0) + (first[3] != 0 ? 1 : 0);
        int secondErrors = (second[1] != 0 ? 1 : 0) + (second[3] != 0 ? 1 : 0);
        return secondErrors < firstErrors ? second : first;
    }

    // Returns the executable path behind a live process handle, or null.
    public static string ImagePath(IntPtr process) {
        if (process == IntPtr.Zero) { return null; }
        StringBuilder buffer = new StringBuilder(2048);
        uint size = (uint)buffer.Capacity;
        if (!QueryFullProcessImageName(process, 0, buffer, ref size)) { return null; }
        return buffer.ToString();
    }
}
'@
}

# ---------------------------------------------------------------------------
# Generic helpers
# ---------------------------------------------------------------------------

function Time-UtcMs {
    return [long]([DateTime]::UtcNow - [DateTime]::UnixEpoch).TotalMilliseconds
}

function Get-JsonField {
    param([object]$Object, [string]$Name)
    if ($null -eq $Object) { return $null }
    return $Object.PSObject.Properties[$Name].Value
}

function Test-JsonField {
    param([object]$Object, [string]$Name)
    if ($null -eq $Object) { return $false }
    return ($null -ne $Object.PSObject.Properties[$Name])
}

function Test-IntValue {
    param([object]$Value)
    if ($null -eq $Value) { return $false }
    if ($Value -is [bool]) { return $false }
    if (($Value -is [int]) -or ($Value -is [long]) -or ($Value -is [short]) -or ($Value -is [byte]) -or ($Value -is [uint32]) -or ($Value -is [uint64])) { return $true }
    if (($Value -is [double]) -or ($Value -is [float]) -or ($Value -is [decimal])) {
        $number = [double]$Value
        if ([double]::IsNaN($number)) { return $false }
        if ([double]::IsInfinity($number)) { return $false }
        return ($number -eq [Math]::Floor($number))
    }
    return $false
}

function Test-NumberValue {
    param([object]$Value)
    if ($null -eq $Value) { return $false }
    if ($Value -is [bool]) { return $false }
    return ($Value -is [int]) -or ($Value -is [long]) -or ($Value -is [short]) -or ($Value -is [byte]) -or ($Value -is [double]) -or ($Value -is [float]) -or ($Value -is [decimal]) -or ($Value -is [uint32]) -or ($Value -is [uint64])
}

function Format-Number {
    param([object]$Value, [int]$Decimals = 3)
    if ($null -eq $Value) { return '' }
    if (-not (Test-NumberValue $Value)) { return [string]$Value }
    return ([double]$Value).ToString('F' + $Decimals, [System.Globalization.CultureInfo]::InvariantCulture)
}

function Format-Mib {
    param([object]$Bytes, [int]$Decimals = 2)
    if ($null -eq $Bytes) { return '' }
    if (-not (Test-NumberValue $Bytes)) { return '' }
    return ([double]$Bytes / 1048576.0).ToString('F' + $Decimals, [System.Globalization.CultureInfo]::InvariantCulture)
}

function Format-SizeValue {
    param([object]$Value)
    if ($null -eq $Value) { return '' }
    if ($Value -is [string]) { return $Value }
    if (Test-NumberValue $Value) { return (Format-Number $Value 0) }
    if ($Value -is [System.Management.Automation.PSCustomObject]) {
        $width = Get-JsonField $Value 'width'
        $height = Get-JsonField $Value 'height'
        if ((Test-NumberValue $width) -and (Test-NumberValue $height)) {
            return (Format-Number $width 0) + 'x' + (Format-Number $height 0)
        }
        return [string]$Value
    }
    $items = @($Value)
    if ($items.Count -eq 2) {
        return (Format-Number $items[0] 0) + 'x' + (Format-Number $items[1] 0)
    }
    return [string]$Value
}

function Get-ConfigValue {
    param([object]$Config, [string]$Name)
    if ($null -eq $Config) { return $null }
    if ($Config -is [System.Collections.IDictionary]) {
        if ($Config.Contains($Name)) { return $Config[$Name] }
        return $null
    }
    return (Get-JsonField $Config $Name)
}

function Get-BucketValue {
    param([object]$Bucket, [string]$Name)
    if ($null -eq $Bucket) { return $null }
    return $Bucket[$Name]
}

function Format-CsvText {
    param([object]$Value)
    $text = ''
    if ($null -ne $Value) { $text = [string]$Value }
    $text = $text.Replace("`r", ' ').Replace("`n", ' ')
    return ('"' + $text.Replace('"', '""') + '"')
}

function Add-Reason {
    param([object]$List, [string]$Text)
    [void]$List.Add($Text)
}

function Add-Note {
    param([object]$List, [string]$Text)
    [void]$List.Add($Text)
}

function Write-NewUtf8File {
    param([string]$Path, [string]$Text)
    if ([System.IO.File]::Exists($Path)) { throw ('Refusing to overwrite existing artifact: ' + $Path) }
    [System.IO.File]::WriteAllText($Path, $Text, [System.Text.UTF8Encoding]::new($false))
}

function Get-UniqueStrings {
    param([object]$List)
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    $result = [System.Collections.Generic.List[object]]::new()
    foreach ($item in @($List)) {
        $text = [string]$item
        if ($seen.Add($text)) { [void]$result.Add($text) }
    }
    return @($result)
}

function Format-MarkdownCell {
    param([object]$Value)
    $text = ''
    if ($null -ne $Value) { $text = ([string]$Value).Replace("`r", ' ').Replace("`n", ' ') }
    return $text.Replace('|', '\|')
}

function Format-MarkdownTable {
    param([string[]]$Headers, [object[]]$Rows)
    $lines = [System.Collections.Generic.List[object]]::new()
    $headerCells = @()
    foreach ($header in $Headers) { $headerCells += (Format-MarkdownCell $header) }
    [void]$lines.Add('| ' + ($headerCells -join ' | ') + ' |')
    $separator = @()
    foreach ($header in $Headers) { $separator += '---' }
    [void]$lines.Add('| ' + ($separator -join ' | ') + ' |')
    foreach ($row in $Rows) {
        $cells = @()
        foreach ($cell in @($row)) { $cells += (Format-MarkdownCell $cell) }
        [void]$lines.Add('| ' + ($cells -join ' | ') + ' |')
    }
    return ($lines -join "`n")
}

# ---------------------------------------------------------------------------
# Typed JSON field extraction. Every miss or type error is recorded once in
# the supplied reason list; nothing is silently coerced to 0.
# ---------------------------------------------------------------------------

function Get-ValidatedInt {
    param([object]$Object, [string]$Name, [string]$Prefix, [object]$Reasons)
    if (-not (Test-JsonField $Object $Name)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' missing')
        return $null
    }
    $value = Get-JsonField $Object $Name
    if (-not (Test-IntValue $value)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' is not an integer')
        return $null
    }
    return [long]$value
}

function Get-OptionalInt {
    param([object]$Object, [string]$Name, [string]$Prefix, [object]$Reasons)
    if (-not (Test-JsonField $Object $Name)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' missing')
        return $null
    }
    $value = Get-JsonField $Object $Name
    if ($null -eq $value) { return $null }
    if (-not (Test-IntValue $value)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' is not an integer')
        return $null
    }
    return [long]$value
}

function Get-ValidatedNumber {
    param([object]$Object, [string]$Name, [string]$Prefix, [object]$Reasons)
    if (-not (Test-JsonField $Object $Name)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' missing')
        return $null
    }
    $value = Get-JsonField $Object $Name
    if (-not (Test-NumberValue $value)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' is not a number')
        return $null
    }
    $number = [double]$value
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number) -or $number -lt 0) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' is not finite and nonnegative')
        return $null
    }
    return $number
}

function Get-OptionalNumber {
    param([object]$Object, [string]$Name, [string]$Prefix, [object]$Reasons)
    if (-not (Test-JsonField $Object $Name)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' missing')
        return $null
    }
    $value = Get-JsonField $Object $Name
    if ($null -eq $value) { return $null }
    if (-not (Test-NumberValue $value)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' is not a number')
        return $null
    }
    $number = [double]$value
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) {
        Add-Reason $Reasons ($Prefix + '.' + $Name + ' is not finite')
        return $null
    }
    return $number
}

function Get-JsonMetadata {
    param(
        [object]$Object,
        [string]$Name,
        [string]$Kind,
        [string]$Prefix,
        [object]$Reasons,
        [object]$Notes,
        [bool]$NoteMissing = $false,
        [bool]$Required = $false
    )
    if (-not (Test-JsonField $Object $Name)) {
        if ($Required) { Add-Reason $Reasons ($Prefix + '.' + $Name + ' missing') }
        elseif ($NoteMissing) { Add-Note $Notes ($Prefix + '.' + $Name + ' not reported') }
        return $null
    }
    $value = Get-JsonField $Object $Name
    if ($null -eq $value) {
        if ($Required) { Add-Reason $Reasons ($Prefix + '.' + $Name + ' is null') }
        elseif ($NoteMissing) { Add-Note $Notes ($Prefix + '.' + $Name + ' is null') }
        return $null
    }
    switch ($Kind) {
        'size' {
            if ($value -is [System.Collections.IList] -and $value.Count -eq 2) {
                $validSize = $true
                foreach ($component in $value) {
                    if (-not (Test-NumberValue $component) -or [double]$component -le 0 -or [double]::IsNaN([double]$component) -or [double]::IsInfinity([double]$component)) { $validSize = $false }
                }
                if ($validSize) { return $value }
            }
        }
        'positive_number' {
            if ((Test-NumberValue $value) -and [double]$value -gt 0 -and -not [double]::IsNaN([double]$value) -and -not [double]::IsInfinity([double]$value)) { return [double]$value }
        }
        'number' {
            if (Test-NumberValue $value) { return [double]$value }
        }
        'scalar' {
            if ($value -is [string]) { return [string]$value }
            if (Test-NumberValue $value) { return ([string]$value) }
            if ($value -is [bool]) { return ([string]$value).ToLowerInvariant() }
        }
        'bool' {
            if ($value -is [bool]) { return [bool]$value }
        }
        'string' {
            if ($value -is [string] -and -not [string]::IsNullOrWhiteSpace($value)) { return [string]$value }
        }
        'any' {
            return $value
        }
    }
    Add-Reason $Reasons ($Prefix + '.' + $Name + ' has an unexpected type')
    return $null
}

function Read-IntFieldSet {
    param([object]$Object, [string]$Prefix, [string[]]$Names, [object]$Reasons)
    $result = [ordered]@{}
    foreach ($name in $Names) {
        $result[$name] = Get-ValidatedInt -Object $Object -Name $name -Prefix $Prefix -Reasons $Reasons
    }
    return $result
}

# ---------------------------------------------------------------------------
# Stdout protocol parsing
# ---------------------------------------------------------------------------

function Get-BenchProtocol {
    param([string]$StdoutText)
    $protocol = [ordered]@{
        lines = 0
        event_lines = 0
        terminal_lines = 0
        other_lines = 0
        bom_stripped = $false
        event_parse_errors = 0
        terminal_json_error = $false
        terminal = $null
        terminal_raw = $null
        phase_events = [System.Collections.Generic.List[object]]::new()
        snapshots = [System.Collections.Generic.List[object]]::new()
        reload_events = [System.Collections.Generic.List[object]]::new()
        failure_events = [System.Collections.Generic.List[object]]::new()
        ordered_events = [System.Collections.Generic.List[object]]::new()
        phase_windows = [ordered]@{}
        reasons = [System.Collections.Generic.List[object]]::new()
        notes = [System.Collections.Generic.List[object]]::new()
    }
    $reasons = $protocol['reasons']
    $lineNumber = 0
    foreach ($raw in ($StdoutText -split "`n")) {
        $lineNumber++
        $line = $raw
        if ($line.Length -gt 0 -and [int][char]$line[0] -eq 0xFEFF) {
            $protocol['bom_stripped'] = $true
            $line = $line.Substring(1)
        }
        $line = $line.TrimEnd([char]0x000D)
        if ($line.Length -eq 0) { continue }
        $protocol['lines'] = [int]$protocol['lines'] + 1
        if ($line.StartsWith('BENCH_EVENT ', [System.StringComparison]::Ordinal)) {
            $protocol['event_lines'] = [int]$protocol['event_lines'] + 1
            $event = $null
            try { $event = $line.Substring(12) | ConvertFrom-Json -ErrorAction Stop } catch { $event = $null }
            if ($null -eq $event) {
                $protocol['event_parse_errors'] = [int]$protocol['event_parse_errors'] + 1
                Add-Reason $reasons ('malformed BENCH_EVENT JSON on stdout line ' + $lineNumber)
                continue
            }
            $kind = Get-JsonField $event 'event'
            if (-not ($kind -is [string]) -or [string]::IsNullOrWhiteSpace([string]$kind)) {
                Add-Reason $reasons ('BENCH_EVENT without an event name on stdout line ' + $lineNumber)
                continue
            }
            switch ([string]$kind) {
                'phase' {
                    $name = Get-JsonField $event 'phase'
                    if (-not ($name -is [string]) -or [string]::IsNullOrWhiteSpace([string]$name)) {
                        Add-Reason $reasons ('phase event without a name on stdout line ' + $lineNumber)
                    } else {
                        $issues = [System.Collections.Generic.List[object]]::new()
                        if ((Get-JsonField $event 'schema_version') -ne 1) { Add-Reason $issues 'schema_version is not 1' }
                        $record = [ordered]@{
                            name = [string]$name
                            utc_ms = Get-ValidatedInt -Object $event -Name 'utc_ms' -Prefix 'phase' -Reasons $issues
                            elapsed_s = (Get-JsonMetadata -Object $event -Name 'elapsed_s' -Kind 'number' -Prefix 'phase' -Reasons $issues -Notes $protocol['notes'] -NoteMissing $true)
                        }
                        foreach ($issue in $issues) { Add-Reason $reasons ($issue + ' (stdout line ' + $lineNumber + ')') }
                        [void]$protocol['phase_events'].Add($record)
                        [void]$protocol['ordered_events'].Add([ordered]@{ kind = 'phase'; data = $record })
                    }
                }
                'snapshot' {
                    $issues = [System.Collections.Generic.List[object]]::new()
                    if ((Get-JsonField $event 'schema_version') -ne 1) {
                        Add-Reason $issues 'schema_version is not 1'
                    }
                    $snapshot = [ordered]@{
                        line = $lineNumber
                        utc_ms = Get-ValidatedInt -Object $event -Name 'utc_ms' -Prefix 'snapshot' -Reasons $issues
                        elapsed_s = Get-OptionalNumber -Object $event -Name 'elapsed_s' -Prefix 'snapshot' -Reasons $issues
                        phase = ''
                        instance_count = Get-ValidatedInt -Object $event -Name 'instance_count' -Prefix 'snapshot' -Reasons $issues
                        sprite_instances = Get-ValidatedInt -Object $event -Name 'sprite_instances' -Prefix 'snapshot' -Reasons $issues
                        skeleton_instances = Get-ValidatedInt -Object $event -Name 'skeleton_instances' -Prefix 'snapshot' -Reasons $issues
                        cpu_assets = Get-ValidatedInt -Object $event -Name 'cpu_assets' -Prefix 'snapshot' -Reasons $issues
                        gpu_assets = Get-ValidatedInt -Object $event -Name 'gpu_assets' -Prefix 'snapshot' -Reasons $issues
                        expected_assets = Get-ValidatedInt -Object $event -Name 'expected_assets' -Prefix 'snapshot' -Reasons $issues
                        mask_bytes = Get-ValidatedInt -Object $event -Name 'mask_bytes' -Prefix 'snapshot' -Reasons $issues
                        sprite_vertex_capacity_bytes = Get-ValidatedInt -Object $event -Name 'sprite_vertex_capacity_bytes' -Prefix 'snapshot' -Reasons $issues
                        instance_capacity_bytes = Get-ValidatedInt -Object $event -Name 'instance_capacity_bytes' -Prefix 'snapshot' -Reasons $issues
                        palette_capacity_bytes = Get-ValidatedInt -Object $event -Name 'palette_capacity_bytes' -Prefix 'snapshot' -Reasons $issues
                        pending = Get-ValidatedInt -Object $event -Name 'pending' -Prefix 'snapshot' -Reasons $issues
                        region_mode = ''
                        issues = $issues
                    }
                    $phaseName = Get-JsonField $event 'phase'
                    if ($phaseName -is [string]) { $snapshot['phase'] = [string]$phaseName } else { Add-Reason $issues 'phase is not a string' }
                    $regionMode = Get-JsonField $event 'region_mode'
                    if ($regionMode -is [string]) { $snapshot['region_mode'] = [string]$regionMode } else { Add-Reason $issues 'region_mode is not a string' }
                    [void]$protocol['snapshots'].Add($snapshot)
                }
                'reload_complete' {
                    $issues = [System.Collections.Generic.List[object]]::new()
                    if ((Get-JsonField $event 'schema_version') -ne 1) { Add-Reason $issues 'schema_version is not 1' }
                    $reload = [ordered]@{
                        line = $lineNumber
                        request = Get-ValidatedInt -Object $event -Name 'request' -Prefix 'reload_complete' -Reasons $issues
                        instance = Get-ValidatedInt -Object $event -Name 'instance' -Prefix 'reload_complete' -Reasons $issues
                        asset = Get-ValidatedInt -Object $event -Name 'asset' -Prefix 'reload_complete' -Reasons $issues
                        cycle = Get-ValidatedInt -Object $event -Name 'cycle' -Prefix 'reload_complete' -Reasons $issues
                        utc_ms = Get-ValidatedInt -Object $event -Name 'utc_ms' -Prefix 'reload_complete' -Reasons $issues
                        elapsed_s = Get-OptionalNumber -Object $event -Name 'elapsed_s' -Prefix 'reload_complete' -Reasons $issues
                        cpu_assets = Get-ValidatedInt -Object $event -Name 'cpu_assets' -Prefix 'reload_complete' -Reasons $issues
                        gpu_assets = Get-ValidatedInt -Object $event -Name 'gpu_assets' -Prefix 'reload_complete' -Reasons $issues
                        expected_assets = Get-ValidatedInt -Object $event -Name 'expected_assets' -Prefix 'reload_complete' -Reasons $issues
                        issues = $issues
                    }
                    [void]$protocol['reload_events'].Add($reload)
                    [void]$protocol['ordered_events'].Add([ordered]@{ kind = 'reload'; data = $reload })
                }
                'failure' {
                    $issues = [System.Collections.Generic.List[object]]::new()
                    if ((Get-JsonField $event 'schema_version') -ne 1) { Add-Reason $issues 'schema_version is not 1' }
                    $failure = [ordered]@{
                        phase = [string](Get-JsonField $event 'phase')
                        reason = [string](Get-JsonField $event 'error')
                        utc_ms = Get-OptionalInt -Object $event -Name 'utc_ms' -Prefix 'failure' -Reasons $issues
                    }
                    [void]$protocol['failure_events'].Add($failure)
                    foreach ($issue in $issues) { Add-Reason $reasons ($issue + ' (stdout line ' + $lineNumber + ')') }
                }
                default {
                    [void]$protocol['notes'].Add('ignored unknown BENCH_EVENT kind "' + [string]$kind + '"')
                }
            }
        } elseif ($line.StartsWith('BENCH_RESULT ', [System.StringComparison]::Ordinal)) {
            $protocol['terminal_lines'] = [int]$protocol['terminal_lines'] + 1
            if ([int]$protocol['terminal_lines'] -gt 1) {
                Add-Reason $reasons ('more than one BENCH_RESULT line (stdout line ' + $lineNumber + ')')
                continue
            }
            $json = $line.Substring(13)
            $protocol['terminal_raw'] = $json
            $terminal = $null
            try { $terminal = $json | ConvertFrom-Json -ErrorAction Stop } catch { $terminal = $null }
            if ($null -eq $terminal) {
                $protocol['terminal_json_error'] = $true
                Add-Reason $reasons 'BENCH_RESULT is not valid JSON'
            } else {
                $protocol['terminal'] = $terminal
            }
        } elseif ($line.StartsWith('BENCH_', [System.StringComparison]::Ordinal)) {
            Add-Reason $reasons ('unexpected BENCH line on stdout line ' + $lineNumber + ': ' + $line.Substring(0, [Math]::Min(60, $line.Length)))
        } else {
            $protocol['other_lines'] = [int]$protocol['other_lines'] + 1
        }
    }

    # Phase windows: the first event with a name opens the window, later events
    # with the same name extend it. Reload events between phase events are
    # recorded with the next phase event that follows them.
    $pendingReloads = [System.Collections.Generic.List[object]]::new()
    $windows = $protocol['phase_windows']
    foreach ($item in $protocol['ordered_events']) {
        if ($item['kind'] -eq 'reload') {
            [void]$pendingReloads.Add($item['data'])
            continue
        }
        $record = $item['data']
        if ($pendingReloads.Count -gt 0) {
            foreach ($event in $pendingReloads) {
                [void]$protocol['notes'].Add('reload request ' + [string]$event['request'] + ' completed before phase "' + [string]$record['name'] + '"')
            }
            $pendingReloads.Clear()
        }
        $name = [string]$record['name']
        if ($windows.Contains($name)) {
            $window = $windows[$name]
            $window['events'] = [int]$window['events'] + 1
            $window['end_utc_ms'] = $record['utc_ms']
            $window['end_elapsed_s'] = $record['elapsed_s']
        } else {
            $windows[$name] = [ordered]@{
                start_utc_ms = $record['utc_ms']
                end_utc_ms = $null
                start_elapsed_s = $record['elapsed_s']
                end_elapsed_s = $null
                events = 1
            }
        }
    }
    return $protocol
}

# ---------------------------------------------------------------------------
# Terminal result extraction
# ---------------------------------------------------------------------------

function Read-BenchResult {
    param([object]$Terminal, [object]$Reasons, [object]$Notes)
    $parsed = [ordered]@{
        schema_version = $null
        valid_present = $false
        valid = $null
        completion_present = $false
        completion = $null
        failure_present = $false
        failure = $null
        config = [ordered]@{}
        phases = [System.Collections.Generic.List[object]]::new()
        measured_seconds = $null
        scene = [ordered]@{}
        simulation = [ordered]@{}
        regions = [ordered]@{}
        stages = [ordered]@{}
        reloads = [ordered]@{}
        sampler = [ordered]@{}
        gpu_execution_ms_present = $false
        gpu_execution_ms = $null
    }

    $parsed['schema_version'] = Get-ValidatedInt -Object $Terminal -Name 'schema_version' -Prefix 'terminal' -Reasons $Reasons
    if ($null -ne $parsed['schema_version'] -and [long]$parsed['schema_version'] -ne 1) {
        Add-Reason $Reasons ('terminal schema_version ' + $parsed['schema_version'] + ' is not 1')
    }

    if (Test-JsonField $Terminal 'valid') {
        $parsed['valid_present'] = $true
        $value = Get-JsonField $Terminal 'valid'
        if ($value -is [bool]) { $parsed['valid'] = [bool]$value }
        else { Add-Reason $Reasons 'terminal valid is not a boolean' }
    } else {
        Add-Reason $Reasons 'terminal valid key missing'
    }

    if (Test-JsonField $Terminal 'completion') {
        $parsed['completion_present'] = $true
        $value = Get-JsonField $Terminal 'completion'
        if ($value -is [string]) { $parsed['completion'] = [string]$value }
        else { Add-Reason $Reasons 'terminal completion is not a string' }
    } else {
        Add-Reason $Reasons 'terminal completion key missing'
    }

    if (Test-JsonField $Terminal 'failure') {
        $parsed['failure_present'] = $true
        $value = Get-JsonField $Terminal 'failure'
        if ($null -eq $value) {
            $parsed['failure'] = $null
        } elseif ($value -is [string]) {
            $parsed['failure'] = [string]$value
        } else {
            Add-Reason $Reasons 'terminal failure is neither null nor a string'
        }
    } else {
        Add-Reason $Reasons 'terminal failure key missing'
    }

    # config
    $config = Get-JsonField $Terminal 'config'
    if ($null -eq $config) {
        Add-Reason $Reasons 'terminal config missing'
    } else {
        $parsed['config'] = [ordered]@{
            # Configuration facts are required for a comparable baseline.
            instance_count = Get-ValidatedInt -Object $config -Name 'instance_count' -Prefix 'config' -Reasons $Reasons
            mode = $null
            seconds = Get-OptionalInt -Object $config -Name 'seconds' -Prefix 'config' -Reasons $Reasons
            reloads = Get-OptionalInt -Object $config -Name 'reloads' -Prefix 'config' -Reasons $Reasons
            build_profile = $null
            quick = $null
            executable = $null
            workdir = $null
            warmup_seconds = Get-OptionalNumber -Object $config -Name 'warmup_seconds' -Prefix 'config' -Reasons $Reasons
            wander = (Get-JsonMetadata -Object $config -Name 'wander' -Kind 'bool' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            device_type = (Get-JsonMetadata -Object $config -Name 'device_type' -Kind 'string' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            requested_logical_size = (Get-JsonMetadata -Object $config -Name 'requested_logical_size' -Kind 'size' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            logical_size = (Get-JsonMetadata -Object $config -Name 'logical_size' -Kind 'size' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            physical_size = (Get-JsonMetadata -Object $config -Name 'physical_size' -Kind 'size' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            control_physical_size = (Get-JsonMetadata -Object $config -Name 'control_physical_size' -Kind 'size' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            scale_factor = (Get-JsonMetadata -Object $config -Name 'scale_factor' -Kind 'positive_number' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            control_scale_factor = (Get-JsonMetadata -Object $config -Name 'control_scale_factor' -Kind 'positive_number' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
            format = (Get-JsonMetadata -Object $config -Name 'format' -Kind 'string' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
        }
        $mode = Get-JsonField $config 'mode'
        if ($mode -is [string]) { $parsed['config']['mode'] = [string]$mode }
        else { Add-Reason $Reasons 'config.mode missing or not a string' }
        $profile = Get-JsonField $config 'build_profile'
        if ($profile -is [string]) { $parsed['config']['build_profile'] = [string]$profile }
        else { Add-Reason $Reasons 'config.build_profile missing or not a string' }
        $quick = Get-JsonField $config 'quick'
        if ($quick -is [bool]) { $parsed['config']['quick'] = [bool]$quick }
        $executable = Get-JsonField $config 'executable'
        if ($executable -is [string]) { $parsed['config']['executable'] = [string]$executable }
        $workdir = Get-JsonField $config 'workdir'
        if ($workdir -is [string]) { $parsed['config']['workdir'] = [string]$workdir }
        $parsed['config']['adapter'] = (Get-JsonMetadata -Object $config -Name 'adapter' -Kind 'string' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
        $parsed['config']['backend'] = (Get-JsonMetadata -Object $config -Name 'backend' -Kind 'string' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
        $parsed['config']['present_mode'] = (Get-JsonMetadata -Object $config -Name 'present_mode' -Kind 'string' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
        $parsed['config']['alpha_mode'] = (Get-JsonMetadata -Object $config -Name 'alpha_mode' -Kind 'string' -Prefix 'config' -Reasons $Reasons -Notes $Notes -Required $true)
    }

    # measured_seconds is the actual measured phase sum for both case kinds.
    if (Test-JsonField $Terminal 'measured_seconds') {
        $parsed['measured_seconds'] = Get-OptionalNumber -Object $Terminal -Name 'measured_seconds' -Prefix 'terminal' -Reasons $Reasons
    } else {
        Add-Reason $Reasons 'terminal measured_seconds missing'
    }

    # phases
    if (-not (Test-JsonField $Terminal 'phases')) {
        Add-Reason $Reasons 'terminal phases missing'
    } else {
        $phaseItems = @(Get-JsonField $Terminal 'phases')
        if ($phaseItems.Count -eq 0) { Add-Reason $Reasons 'terminal phases is empty' }
        $index = 0
        foreach ($item in $phaseItems) {
            $prefix = 'phases[' + $index + ']'
            $name = Get-JsonField $item 'phase'
            if (-not ($name -is [string]) -or [string]::IsNullOrWhiteSpace([string]$name)) {
                Add-Reason $Reasons ($prefix + '.phase missing or not a string')
            } else {
                $entry = [ordered]@{
                    name = [string]$name
                    start_utc_ms = Get-ValidatedInt -Object $item -Name 'start_utc_ms' -Prefix $prefix -Reasons $Reasons
                    end_utc_ms = Get-ValidatedInt -Object $item -Name 'end_utc_ms' -Prefix $prefix -Reasons $Reasons
                    start_elapsed_s = Get-ValidatedNumber -Object $item -Name 'start_elapsed_s' -Prefix $prefix -Reasons $Reasons
                    end_elapsed_s = Get-ValidatedNumber -Object $item -Name 'end_elapsed_s' -Prefix $prefix -Reasons $Reasons
                }
                if ($null -ne $entry['start_utc_ms'] -and $null -ne $entry['end_utc_ms'] -and $entry['end_utc_ms'] -lt $entry['start_utc_ms']) {
                    Add-Reason $Reasons ($prefix + ' end_utc_ms precedes start_utc_ms')
                }
                if ($null -ne $entry['start_elapsed_s'] -and $null -ne $entry['end_elapsed_s'] -and $entry['end_elapsed_s'] -lt $entry['start_elapsed_s']) {
                    Add-Reason $Reasons ($prefix + ' end_elapsed_s precedes start_elapsed_s')
                }
                [void]$parsed['phases'].Add($entry)
            }
            $index++
        }
    }

    # scene
    $scene = Get-JsonField $Terminal 'scene'
    if ($null -eq $scene) {
        Add-Reason $Reasons 'terminal scene missing'
    } else {
        $parsed['scene'] = Read-IntFieldSet -Object $scene -Prefix 'scene' -Names @('attempts', 'submitted', 'skipped', 'errors') -Reasons $Reasons
        $parsed['scene']['submitted_per_second'] = Get-ValidatedNumber -Object $scene -Name 'submitted_per_second' -Prefix 'scene' -Reasons $Reasons
        $parsed['scene']['skip_reasons'] = [ordered]@{}
        $parsed['scene']['skip_reason_sum'] = $null
        if (-not (Test-JsonField $scene 'skip_reasons')) {
            Add-Reason $Reasons 'scene.skip_reasons missing'
        } else {
            $skipReasons = Get-JsonField $scene 'skip_reasons'
            if ($null -eq $skipReasons) {
                Add-Reason $Reasons 'scene.skip_reasons is null'
            } else {
                $sum = [long]0
                $malformed = 0
                foreach ($property in $skipReasons.PSObject.Properties) {
                    if ((Test-IntValue $property.Value) -and ([long]$property.Value -ge 0)) {
                        $parsed['scene']['skip_reasons'][$property.Name] = [long]$property.Value
                        $sum = $sum + [long]$property.Value
                    } else {
                        Add-Reason $Reasons ('scene.skip_reasons.' + $property.Name + ' is not a nonnegative integer')
                        $malformed++
                    }
                }
                if ($malformed -eq 0) { $parsed['scene']['skip_reason_sum'] = $sum }
            }
        }
        $attempts = $parsed['scene']['attempts']
        $submitted = $parsed['scene']['submitted']
        $skipped = $parsed['scene']['skipped']
        $errors = $parsed['scene']['errors']
        if ($null -ne $attempts -and $null -ne $submitted -and $null -ne $skipped) {
            if ([long]$attempts -ne ([long]$submitted + [long]$skipped)) {
                Add-Reason $Reasons ('scene conservation broken: attempts=' + $attempts + ' submitted=' + $submitted + ' skipped=' + $skipped)
            }
        }
        if ($null -ne $submitted -and [long]$submitted -le 0) {
            Add-Reason $Reasons 'scene.submitted is not positive; no scene frame was submitted'
        }
        if ($null -ne $errors -and [long]$errors -ne 0) {
            Add-Reason $Reasons ('scene.errors is ' + $errors + ', expected 0')
        }
        if ($null -ne $parsed['scene']['skip_reason_sum'] -and $null -ne $skipped -and [long]$parsed['scene']['skip_reason_sum'] -ne [long]$skipped) {
            Add-Note $Notes ('skip reasons sum to ' + $parsed['scene']['skip_reason_sum'] + ' while scene.skipped is ' + $skipped)
        }
    }

    # simulation
    $simulation = Get-JsonField $Terminal 'simulation'
    if ($null -eq $simulation) {
        Add-Reason $Reasons 'terminal simulation missing'
    } else {
        $parsed['simulation'] = Read-IntFieldSet -Object $simulation -Prefix 'simulation' -Names @('steps', 'dropped_steps') -Reasons $Reasons
    }

    # regions
    $regions = Get-JsonField $Terminal 'regions'
    if ($null -eq $regions) {
        Add-Reason $Reasons 'terminal regions missing'
    } else {
        $parsed['regions'] = Read-IntFieldSet -Object $regions -Prefix 'regions' -Names @('rebuilt', 'reused', 'failed', 'polygon_max') -Reasons $Reasons
        $failed = $parsed['regions']['failed']
        if ($null -ne $failed -and [long]$failed -ne 0) {
            Add-Reason $Reasons ('regions.failed is ' + $failed + ', expected 0 (no hit-region fallback)')
        }
    }

    # stages
    $stages = Get-JsonField $Terminal 'stages'
    if ($null -eq $stages) {
        Add-Reason $Reasons 'terminal stages missing'
    } else {
        $timingFields = @('mean_ms', 'p50_ms', 'p95_ms', 'p99_ms', 'max_ms')
        foreach ($name in $script:StageNames) {
            $prefix = 'stages.' + $name
            if (-not (Test-JsonField $stages $name)) {
                Add-Reason $Reasons ($prefix + ' missing')
                $parsed['stages'][$name] = $null
                continue
            }
            $stage = Get-JsonField $stages $name
            $entry = [ordered]@{
                count = Get-ValidatedInt -Object $stage -Name 'count' -Prefix $prefix -Reasons $Reasons
                mean_ms = $null
                p50_ms = $null
                p95_ms = $null
                p99_ms = $null
                max_ms = $null
            }
            $presentButNull = [System.Collections.Generic.List[object]]::new()
            foreach ($field in $timingFields) {
                if (-not (Test-JsonField $stage $field)) {
                    Add-Reason $Reasons ($prefix + '.' + $field + ' missing')
                    continue
                }
                $value = Get-JsonField $stage $field
                if ($null -eq $value) {
                    [void]$presentButNull.Add($field)
                    continue
                }
                if (-not (Test-NumberValue $value)) {
                    Add-Reason $Reasons ($prefix + '.' + $field + ' is not a number')
                    continue
                }
                $number = [double]$value
                if ([double]::IsNaN($number) -or [double]::IsInfinity($number) -or $number -lt 0) {
                    Add-Reason $Reasons ($prefix + '.' + $field + ' is not finite and nonnegative')
                    continue
                }
                $entry[$field] = $number
            }
            if ($null -ne $entry['count'] -and [long]$entry['count'] -eq 0) {
                foreach ($field in $timingFields) {
                    if ($null -ne $entry[$field]) {
                        Add-Reason $Reasons ($prefix + ' has count 0 but ' + $field + ' is not null')
                    }
                }
            } elseif ($null -ne $entry['count'] -and [long]$entry['count'] -gt 0) {
                foreach ($field in $presentButNull) {
                    Add-Reason $Reasons ($prefix + ' has count ' + $entry['count'] + ' but ' + $field + ' is null')
                }
            }
            $parsed['stages'][$name] = $entry
        }
    }

    # reloads
    $reloads = Get-JsonField $Terminal 'reloads'
    if ($null -eq $reloads) {
        Add-Reason $Reasons 'terminal reloads missing'
    } else {
        $parsed['reloads'] = [ordered]@{
            requested = Get-ValidatedInt -Object $reloads -Name 'requested' -Prefix 'reloads' -Reasons $Reasons
            successful = Get-ValidatedInt -Object $reloads -Name 'successful' -Prefix 'reloads' -Reasons $Reasons
            render_confirmed = Get-ValidatedInt -Object $reloads -Name 'render_confirmed' -Prefix 'reloads' -Reasons $Reasons
            expected = Get-OptionalInt -Object $reloads -Name 'expected' -Prefix 'reloads' -Reasons $Reasons
        }
    }

    # sampler
    $sampler = Get-JsonField $Terminal 'sampler'
    if ($null -eq $sampler) {
        Add-Reason $Reasons 'terminal sampler missing'
    } else {
        $parsed['sampler'] = Read-IntFieldSet -Object $sampler -Prefix 'sampler' -Names @('scene_capacity', 'control_capacity', 'allocated_bytes', 'scene_used', 'control_used') -Reasons $Reasons
        $sceneCapacity = $parsed['sampler']['scene_capacity']
        $sceneUsed = $parsed['sampler']['scene_used']
        $controlCapacity = $parsed['sampler']['control_capacity']
        $controlUsed = $parsed['sampler']['control_used']
        if ($null -ne $sceneCapacity -and $null -ne $sceneUsed -and [long]$sceneUsed -gt [long]$sceneCapacity) {
            Add-Reason $Reasons ('sampler overflow: scene_used ' + $sceneUsed + ' exceeds scene_capacity ' + $sceneCapacity)
        }
        if ($null -ne $controlCapacity -and $null -ne $controlUsed -and [long]$controlUsed -gt [long]$controlCapacity) {
            Add-Reason $Reasons ('sampler overflow: control_used ' + $controlUsed + ' exceeds control_capacity ' + $controlCapacity)
        }
    }

    # gpu_execution_ms must be present and null
    if (Test-JsonField $Terminal 'gpu_execution_ms') {
        $parsed['gpu_execution_ms_present'] = $true
        $parsed['gpu_execution_ms'] = Get-JsonField $Terminal 'gpu_execution_ms'
    } else {
        Add-Reason $Reasons 'terminal gpu_execution_ms missing'
    }

    return $parsed
}

function Get-PhaseWindowByName {
    param([object]$Phases, [string]$Name)
    foreach ($phase in @($Phases)) {
        if ($phase['name'] -eq $Name) {
            if ($null -ne $phase['start_utc_ms'] -and $null -ne $phase['end_utc_ms']) { return $phase }
            return $null
        }
    }
    return $null
}

function Get-PhaseDurationSeconds {
    param([object]$Phases, [string]$Name)
    foreach ($phase in @($Phases)) {
        if ($phase['name'] -eq $Name) {
            if ($null -ne $phase['start_elapsed_s'] -and $null -ne $phase['end_elapsed_s']) {
                return ([double]$phase['end_elapsed_s'] - [double]$phase['start_elapsed_s'])
            }
            if ($null -ne $phase['start_utc_ms'] -and $null -ne $phase['end_utc_ms']) {
                return (([double]$phase['end_utc_ms'] - [double]$phase['start_utc_ms']) / 1000.0)
            }
            return $null
        }
    }
    return $null
}

function Test-PhasePresent {
    param([object]$Phases, [string]$Name)
    foreach ($phase in @($Phases)) {
        if ($phase['name'] -eq $Name) { return $true }
    }
    return $false
}

# ---------------------------------------------------------------------------
# Per-case acceptance contract
# ---------------------------------------------------------------------------

function Test-BenchCaseContract {
    param([object]$Case, [object]$Parsed, [object]$Protocol, [object]$Reasons, [object]$Notes)

    if ([int]$Protocol['terminal_lines'] -eq 0) {
        Add-Reason $Reasons 'stdout has no BENCH_RESULT terminal line'
    }

    if ($null -eq $Parsed) {
        Add-Reason $Reasons 'terminal result unavailable'
        return
    }

    if (-not $Parsed['valid_present']) {
        # already reported by Read-BenchResult
    } elseif ($null -ne $Parsed['valid'] -and -not $Parsed['valid']) {
        Add-Reason $Reasons 'terminal valid is not true'
    }
    if ($Parsed['completion_present'] -and $null -ne $Parsed['completion'] -and $Parsed['completion'] -ne 'completed') {
        Add-Reason $Reasons ('terminal completion is "' + $Parsed['completion'] + '", expected "completed"')
    }
    if ($Parsed['failure_present'] -and $null -ne $Parsed['failure']) {
        Add-Reason $Reasons ('terminal failure is not null: ' + $Parsed['failure'])
    }

    $config = $Parsed['config']
    if ($null -ne $config) {
        if ($null -ne $config['instance_count'] -and [long]$config['instance_count'] -ne [long]$Case['instance_count']) {
            Add-Reason $Reasons ('config.instance_count ' + $config['instance_count'] + ' does not match the planned ' + $Case['instance_count'])
        }
        if ($null -ne $config['mode'] -and $config['mode'] -ne $Case['mode']) {
            Add-Reason $Reasons ('config.mode "' + $config['mode'] + '" does not match the planned "' + $Case['mode'] + '"')
        }
        if ($null -eq $Case['seconds']) {
            if ($null -ne $config['seconds']) {
                Add-Reason $Reasons ('config.seconds must be null for the reload case (got ' + $config['seconds'] + ')')
            }
        } elseif ($null -eq $config['seconds']) {
            Add-Reason $Reasons ('config.seconds must equal the planned ' + $Case['seconds'])
        } elseif ([Math]::Abs([double]$config['seconds'] - [double]$Case['seconds']) -gt $script:TimeEpsilon) {
            Add-Reason $Reasons ('config.seconds ' + $config['seconds'] + ' does not match the planned ' + $Case['seconds'])
        }
        if ($null -eq $Case['reloads']) {
            if ($null -ne $config['reloads']) {
                Add-Reason $Reasons ('config.reloads must be null for the timed case (got ' + $config['reloads'] + ')')
            }
        } elseif ($null -eq $config['reloads']) {
            Add-Reason $Reasons ('config.reloads must equal the planned ' + $Case['reloads'])
        } elseif ([long]$config['reloads'] -ne [long]$Case['reloads']) {
            Add-Reason $Reasons ('config.reloads ' + $config['reloads'] + ' does not match the planned ' + $Case['reloads'])
        }
        if ($null -ne $config['build_profile'] -and $config['build_profile'] -ne 'release') {
            Add-Reason $Reasons ('config.build_profile "' + $config['build_profile'] + '" is not "release"')
        }
        if ($null -ne $config['wander'] -and [bool]$config['wander'] -ne [bool]$Case['wander']) {
            Add-Reason $Reasons ('config.wander ' + $config['wander'] + ' does not match the planned ' + $Case['wander'])
        }
    }

    if ($Parsed['gpu_execution_ms_present'] -and $null -ne $Parsed['gpu_execution_ms']) {
        Add-Reason $Reasons 'gpu_execution_ms must be null (GPU execution is not measured)'
    }

    # Phase-plan contract.
    $phases = @($Parsed['phases'])
    $warmupDuration = Get-PhaseDurationSeconds -Phases $phases -Name 'warmup'
    if (-not (Test-PhasePresent -Phases $phases -Name 'warmup')) {
        Add-Reason $Reasons 'warmup phase missing from phases'
    } elseif ($null -eq $warmupDuration) {
        Add-Reason $Reasons 'warmup phase has no usable time window'
    } elseif ($warmupDuration -lt ($script:WarmupSeconds - $script:TimeEpsilon)) {
        Add-Reason $Reasons ('warmup phase duration ' + (Format-Number $warmupDuration) + 's is below the required ' + $script:WarmupSeconds + 's')
    } else {
        Add-Note $Notes ('warmup duration ' + (Format-Number $warmupDuration) + 's')
    }

    if ($null -eq $Case['seconds']) {
        if (-not (Test-PhasePresent -Phases $phases -Name 'settle')) {
            Add-Reason $Reasons 'settle phase missing from phases'
        } else {
            $settleDuration = Get-PhaseDurationSeconds -Phases $phases -Name 'settle'
            if ($null -eq $settleDuration) {
                Add-Reason $Reasons 'settle phase has no usable time window'
            } elseif ($settleDuration -lt ($script:SettleSeconds - $script:TimeEpsilon)) {
                Add-Reason $Reasons ('settle phase duration ' + (Format-Number $settleDuration) + 's is below the required ' + $script:SettleSeconds + 's')
            } else {
                Add-Note $Notes ('settle duration ' + (Format-Number $settleDuration) + 's')
            }
        }
        $reloadBaseline = Get-PhaseDurationSeconds -Phases $phases -Name 'reload_baseline'
        if ($null -eq $reloadBaseline) {
            Add-Reason $Reasons 'reload_baseline phase has no usable time window'
        } elseif ($reloadBaseline -lt ($script:ReloadBaselineSeconds - $script:TimeEpsilon)) {
            Add-Reason $Reasons ('reload_baseline phase duration ' + (Format-Number $reloadBaseline) + 's is below the required ' + $script:ReloadBaselineSeconds + 's')
        }
        $reloadDuration = Get-PhaseDurationSeconds -Phases $phases -Name 'reload'
        $minimumReload = [Math]::Max(0, [long]$Case['reloads'] - 1) * $script:MinSecondsPerSuccess
        if ($null -eq $reloadDuration) {
            Add-Reason $Reasons 'reload phase has no usable time window'
        } elseif ($reloadDuration -lt ($minimumReload - $script:TimeEpsilon)) {
            Add-Reason $Reasons ('reload phase duration is below the required request spacing of ' + (Format-Number $minimumReload) + 's')
        }
        $measuredPhaseNames = @('reload_baseline', 'reload', 'settle')
    } else {
        if (-not (Test-PhasePresent -Phases $phases -Name 'measure')) {
            Add-Reason $Reasons 'measure phase missing from phases'
        }
        $measureDuration = Get-PhaseDurationSeconds -Phases $phases -Name 'measure'
        if ($null -eq $measureDuration) {
            Add-Reason $Reasons 'measure phase has no usable time window'
        } elseif ($measureDuration -lt ([double]$Case['seconds'] - $script:TimeEpsilon)) {
            Add-Reason $Reasons ('measure phase duration ' + (Format-Number $measureDuration) + 's is below the planned ' + $Case['seconds'] + 's')
        } else {
            Add-Note $Notes ('measure phase window ' + (Format-Number $measureDuration) + 's')
            if ($measureDuration -gt ([double]$Case['seconds'] + $script:TimeEpsilon)) {
                Add-Note $Notes ('event-loop deadline overshoot ' + (Format-Number ($measureDuration - [double]$Case['seconds'])) + 's')
            }
        }
        $measuredPhaseNames = @('measure')
    }
    $measuredPhaseSum = 0.0
    foreach ($name in $measuredPhaseNames) {
        $matching = @($phases | Where-Object { $_['name'] -eq $name })
        if ($matching.Count -ne 1) {
            Add-Reason $Reasons ('expected exactly one measured phase "' + $name + '", got ' + $matching.Count)
        }
        $duration = Get-PhaseDurationSeconds -Phases $phases -Name $name
        if ($null -ne $duration) { $measuredPhaseSum += $duration }
    }
    if ($null -eq $Parsed['measured_seconds']) {
        Add-Reason $Reasons 'measured_seconds must be a number for every case'
    } elseif ([Math]::Abs([double]$Parsed['measured_seconds'] - $measuredPhaseSum) -gt $script:TimeEpsilon) {
        Add-Reason $Reasons ('measured_seconds ' + (Format-Number $Parsed['measured_seconds']) + ' does not match measured phase sum ' + (Format-Number $measuredPhaseSum) + 's')
    }

    # Reload accounting comes from the child, not from the planned values.
    if ($null -ne $Case['reloads'] -and $null -ne $Parsed['reloads']) {
        $reloads = $Parsed['reloads']
        foreach ($field in @('requested', 'successful', 'render_confirmed', 'expected')) {
            $value = $reloads[$field]
            if ($null -ne $value -and [long]$value -ne [long]$Case['reloads']) {
                Add-Reason $Reasons ('reloads.' + $field + ' is ' + $value + ', expected ' + $Case['reloads'])
            }
        }
        $reloadEvents = @($Protocol['reload_events'])
        if ($reloadEvents.Count -ne [long]$Case['reloads']) {
            Add-Reason $Reasons ('reload_complete events ' + $reloadEvents.Count + ' do not match the planned ' + $Case['reloads'])
        } else {
            # Request ids are opaque app-level ids that include the two setup
            # loads; they must be unique but are not required to start at 1 or
            # be contiguous. The cycle field carries the 1..COUNT completion
            # cycle. The final completion may be emitted after the transition
            # into settle, so no completion phase is required.
            $requests = [System.Collections.Generic.List[long]]::new()
            $cycles = [System.Collections.Generic.List[long]]::new()
            $cycleIssues = 0
            foreach ($event in $reloadEvents) {
                $request = $event['request']
                if ($null -ne $request) { [void]$requests.Add([long]$request) }
                $cycle = $event['cycle']
                if ($null -ne $cycle) { [void]$cycles.Add([long]$cycle) }
                if ($event['issues'].Count -gt 0) { $cycleIssues = $cycleIssues + $event['issues'].Count }
            }
            if ($cycleIssues -gt 0) {
                Add-Reason $Reasons ('reload_complete protocol issues: ' + $cycleIssues)
            }
            if (@($requests | Sort-Object -Unique).Count -ne $requests.Count) {
                Add-Reason $Reasons 'reload request ids are not unique'
            }
            $expectedCycles = @()
            foreach ($number in 1..[int]$Case['reloads']) { $expectedCycles += $number }
            $sortedCycles = @($cycles | Sort-Object)
            $cycleMatches = ($sortedCycles.Count -eq $expectedCycles.Count)
            if ($cycleMatches) {
                for ($i = 0; $i -lt $sortedCycles.Count; $i++) {
                    if ([long]$sortedCycles[$i] -ne [long]$expectedCycles[$i]) { $cycleMatches = $false; break }
                }
            }
            if (-not $cycleMatches) {
                $rangeText = 'none'
                if ($sortedCycles.Count -gt 0) { $rangeText = ([string]$sortedCycles[0] + '..' + [string]$sortedCycles[$sortedCycles.Count - 1]) }
                Add-Reason $Reasons ('reload cycle numbers are not 1..' + $Case['reloads'] + ' (observed ' + $rangeText + ', count ' + $sortedCycles.Count + ')')
            }
        }
    }

    # Snapshots: emitted after production cleanup, so they must reflect the
    # expected instance and asset counts.
    $snapshots = @($Protocol['snapshots'])
    if ($snapshots.Count -eq 0) {
        Add-Reason $Reasons 'no BENCH_EVENT snapshot was emitted'
    } else {
        $snapshotIssues = 0
        $firstIssue = $null
        $mismatchCount = 0
        $firstMismatch = $null
        $instanceMismatch = 0
        foreach ($snapshot in $snapshots) {
            if ($snapshot['issues'].Count -gt 0) {
                $snapshotIssues = $snapshotIssues + $snapshot['issues'].Count
                if ($null -eq $firstIssue) { $firstIssue = $snapshot['issues'][0] }
            }
            if ($null -ne $snapshot['instance_count'] -and [long]$snapshot['instance_count'] -ne [long]$Case['instance_count']) {
                $instanceMismatch++
            }
            if ($null -ne $snapshot['cpu_assets'] -and $null -ne $snapshot['gpu_assets'] -and $null -ne $snapshot['expected_assets']) {
                if (($snapshot['cpu_assets'] -ne $snapshot['gpu_assets']) -or ($snapshot['gpu_assets'] -ne $snapshot['expected_assets'])) {
                    $mismatchCount++
                    if ($null -eq $firstMismatch) {
                        $firstMismatch = ('line ' + $snapshot['line'] + ': instance_count=' + $snapshot['instance_count'] + ' cpu_assets=' + $snapshot['cpu_assets'] + ' gpu_assets=' + $snapshot['gpu_assets'] + ' expected_assets=' + $snapshot['expected_assets'])
                    }
                }
            }
        }
        if ($snapshotIssues -gt 0) {
            Add-Reason $Reasons ('snapshot protocol issues: ' + $snapshotIssues + ' (first: ' + $firstIssue + ')')
        }
        if ($instanceMismatch -gt 0) {
            Add-Reason $Reasons ('snapshot instance_count mismatch in ' + $instanceMismatch + ' of ' + $snapshots.Count + ' snapshots (planned ' + $Case['instance_count'] + ')')
        }
        if ($mismatchCount -gt 0) {
            Add-Reason $Reasons ('snapshot asset mismatch in ' + $mismatchCount + ' of ' + $snapshots.Count + ' snapshots (first ' + $firstMismatch + ')')
        }
    }

    # Failure events are recorded even when the terminal already failed.
    $failureEvents = @($Protocol['failure_events'])
    foreach ($event in $failureEvents) {
        Add-Reason $Reasons ('failure event during phase "' + $event['phase'] + '": ' + $event['reason'])
    }
}

# ---------------------------------------------------------------------------
# Resource sampling
# ---------------------------------------------------------------------------

function Get-GuiReadinessState {
    param([IntPtr]$WindowHandle, [double]$ElapsedSeconds)
    if ($ElapsedSeconds -ge $script:GuiReadyTimeoutSeconds) { return 'timed_out' }
    if ($WindowHandle -ne [IntPtr]::Zero) { return 'ready' }
    return 'waiting'
}

function Get-ChildMetrics {
    param([System.Diagnostics.Process]$Process, [IntPtr]$NativeHandle, [double]$ElapsedSeconds)
    $sample = [ordered]@{
        utc_ms = Time-UtcMs
        elapsed_s = [Math]::Round($ElapsedSeconds, 3)
        ok = $false
        private_bytes = $null
        working_set_bytes = $null
        cpu_seconds = $null
        handle_count = $null
        gdi = $null
        user = $null
        gdi_error = $null
        user_error = $null
        error = $null
    }
    try {
        $Process.Refresh()
        if ($Process.HasExited) { return $null }
        $sample['private_bytes'] = [long]$Process.PrivateMemorySize64
        $sample['working_set_bytes'] = [long]$Process.WorkingSet64
        $sample['cpu_seconds'] = [double]$Process.TotalProcessorTime.TotalSeconds
        $sample['handle_count'] = [long]$Process.HandleCount
        $sample['ok'] = $true
    } catch {
        $exited = $false
        try { $exited = $Process.HasExited } catch { $exited = $true }
        if ($exited) { return $null }
        $sample['error'] = $_.Exception.Message
        return $sample
    }
    try {
        $gui = [BenchNative]::Gui($NativeHandle)
        $sample['gdi_error'] = [int]$gui[1]
        $sample['user_error'] = [int]$gui[3]
        if ([int]$gui[1] -eq 0) { $sample['gdi'] = [int]$gui[0] }
        if ([int]$gui[3] -eq 0) { $sample['user'] = [int]$gui[2] }
    } catch {
        $sample['error'] = $_.Exception.Message
    }
    try {
        if ($Process.HasExited) { return $null }
    } catch {
        $sample['ok'] = $false
        $sample['error'] = 'exit-state query failed: ' + $_.Exception.Message
    }
    return $sample
}

function Test-ProcessIdentity {
    param([System.Diagnostics.Process]$Process, [long]$StartTicks, [IntPtr]$NativeHandle, [string]$ExpectedPath)
    $state = [ordered]@{ ok = $false; error = $null }
    if ($null -eq $Process) {
        $state['error'] = 'process object missing'
        return $state
    }
    $exited = $false
    try { $exited = $Process.HasExited } catch { $exited = $true }
    if ($exited) {
        $state['error'] = 'process already exited'
        return $state
    }
    try {
        if ($Process.StartTime.Ticks -ne $StartTicks) {
            $state['error'] = 'process start time no longer matches the held child'
            return $state
        }
    } catch {
        $state['error'] = 'start time query failed: ' + $_.Exception.Message
        return $state
    }
    try {
        $imagePath = [BenchNative]::ImagePath($NativeHandle)
        if ($null -eq $imagePath) {
            $state['error'] = 'executable path query returned null'
            return $state
        }
        if ($null -ne $ExpectedPath -and -not ($imagePath -ieq $ExpectedPath)) {
            $state['error'] = 'executable path "' + $imagePath + '" does not match "' + $ExpectedPath + '"'
            return $state
        }
    } catch {
        $state['error'] = 'executable path query failed: ' + $_.Exception.Message
        return $state
    }
    $state['ok'] = $true
    return $state
}

function Measure-ResourceSummary {
    param([object]$Samples, [object]$PhaseWindows, [object]$TerminalPhases)
    $summary = [ordered]@{
        samples_attempted = 0
        samples_ok = 0
        samples_failed = 0
        samples_dropped_at_exit = 0
        private = [ordered]@{ start_bytes = $null; end_bytes = $null; peak_bytes = $null; peak_utc_ms = $null }
        working_set = [ordered]@{ start_bytes = $null; end_bytes = $null; peak_bytes = $null; peak_utc_ms = $null }
        handles = [ordered]@{ start = $null; end = $null; peak = $null }
        gdi = [ordered]@{ last = $null; peak = $null; missing_samples = 0; last_error = $null }
        user = [ordered]@{ last = $null; peak = $null; missing_samples = 0; last_error = $null }
        cpu = [ordered]@{ core_seconds = $null; wall_seconds = $null; average_used_cores = $null; logical_core_percent = $null; samples_with_cpu = 0 }
        windows = [ordered]@{ label = $null; span_seconds = $null; first_seconds = $null; last_seconds = $null; first = $null; last = $null; private_delta_bytes = $null; working_set_delta_bytes = $null; cpu_rate_first = $null; cpu_rate_last = $null }
        phases = [ordered]@{}
        anomalies = [System.Collections.Generic.List[object]]::new()
    }
    $anomalies = $summary['anomalies']
    $sampleList = @($Samples)
    $summary['samples_attempted'] = $sampleList.Count
    $okSamples = [System.Collections.Generic.List[object]]::new()
    foreach ($sample in $sampleList) {
        if ($sample['ok'] -eq $true) { [void]$okSamples.Add($sample) }
        else {
            $summary['samples_failed'] = [int]$summary['samples_failed'] + 1
            [void]$anomalies.Add('sample at ' + $sample['utc_ms'] + ' failed: ' + [string]$sample['error'])
        }
        if ($null -ne $sample['gdi_error'] -and [int]$sample['gdi_error'] -ne 0) {
            $summary['gdi']['missing_samples'] = [int]$summary['gdi']['missing_samples'] + 1
            $summary['gdi']['last_error'] = [int]$sample['gdi_error']
            [void]$anomalies.Add('GetGuiResources(GDI) failed with error ' + $sample['gdi_error'] + ' at ' + $sample['utc_ms'])
        }
        if ($null -ne $sample['user_error'] -and [int]$sample['user_error'] -ne 0) {
            $summary['user']['missing_samples'] = [int]$summary['user']['missing_samples'] + 1
            $summary['user']['last_error'] = [int]$sample['user_error']
            [void]$anomalies.Add('GetGuiResources(USER) failed with error ' + $sample['user_error'] + ' at ' + $sample['utc_ms'])
        }
    }
    $summary['samples_ok'] = $okSamples.Count
    if ($okSamples.Count -eq 0) {
        [void]$anomalies.Add('no successful resource samples were collected')
        return $summary
    }
    $first = $okSamples[0]
    $last = $okSamples[$okSamples.Count - 1]

    $summary['private']['start_bytes'] = $first['private_bytes']
    $summary['private']['end_bytes'] = $last['private_bytes']
    $summary['working_set']['start_bytes'] = $first['working_set_bytes']
    $summary['working_set']['end_bytes'] = $last['working_set_bytes']
    $summary['handles']['start'] = $first['handle_count']
    $summary['handles']['end'] = $last['handle_count']
    foreach ($sample in $okSamples) {
        foreach ($pair in @(@('private', 'private_bytes'), @('working_set', 'working_set_bytes'))) {
            $bucket = $pair[0]
            $field = $pair[1]
            $value = $sample[$field]
            if ($null -ne $value) {
                if ($null -eq $summary[$bucket]['peak_bytes'] -or [long]$value -gt [long]$summary[$bucket]['peak_bytes']) {
                    $summary[$bucket]['peak_bytes'] = $value
                    $summary[$bucket]['peak_utc_ms'] = $sample['utc_ms']
                }
            }
        }
        if ($null -ne $sample['handle_count']) {
            if ($null -eq $summary['handles']['peak'] -or [long]$sample['handle_count'] -gt [long]$summary['handles']['peak']) {
                $summary['handles']['peak'] = $sample['handle_count']
            }
        }
        if ($null -ne $sample['gdi']) {
            if ($null -eq $summary['gdi']['peak'] -or [long]$sample['gdi'] -gt [long]$summary['gdi']['peak']) {
                $summary['gdi']['peak'] = $sample['gdi']
            }
            $summary['gdi']['last'] = $sample['gdi']
        }
        if ($null -ne $sample['user']) {
            if ($null -eq $summary['user']['peak'] -or [long]$sample['user'] -gt [long]$summary['user']['peak']) {
                $summary['user']['peak'] = $sample['user']
            }
            $summary['user']['last'] = $sample['user']
        }
        if ($null -ne $sample['cpu_seconds']) {
            $summary['cpu']['samples_with_cpu'] = [int]$summary['cpu']['samples_with_cpu'] + 1
        }
    }

    # CPU arithmetic: core-seconds as the delta of process CPU time, average
    # used cores as the delta over the sampling wall time, and a logical-core
    # normalized percentage reported separately.
    if ($null -ne $first['cpu_seconds'] -and $null -ne $last['cpu_seconds']) {
        $cpuDelta = [double]$last['cpu_seconds'] - [double]$first['cpu_seconds']
        $wallDelta = ([double]$last['utc_ms'] - [double]$first['utc_ms']) / 1000.0
        $summary['cpu']['core_seconds'] = $cpuDelta
        $summary['cpu']['wall_seconds'] = $wallDelta
        if ($cpuDelta -lt 0) {
            [void]$anomalies.Add('CPU time decreased between the first and last sample (counter reset or truncated sampling)')
        } elseif ($wallDelta -le 0) {
            [void]$anomalies.Add('sampling wall time is not positive; average core usage cannot be derived')
        } else {
            $summary['cpu']['average_used_cores'] = $cpuDelta / $wallDelta
            if ($script:LogicalCores -gt 0) {
                $summary['cpu']['logical_core_percent'] = ($cpuDelta / $wallDelta) / [double]$script:LogicalCores * 100.0
            }
        }
    } else {
        [void]$anomalies.Add('CPU time missing on the first or last successful sample')
    }

    # First-vs-last stable windows inside the measured span. Falls back to the
    # whole run when the expected phase names are absent.
    $spanNames = [System.Collections.Generic.List[string]]::new()
    if (Test-PhasePresent -Phases $TerminalPhases -Name 'measure') {
        [void]$spanNames.Add('measure')
    } elseif (Test-PhasePresent -Phases $TerminalPhases -Name 'reload') {
        [void]$spanNames.Add('reload_baseline')
        [void]$spanNames.Add('reload')
    } elseif ((@($TerminalPhases)).Count -gt 0) {
        foreach ($phase in @($TerminalPhases)) { [void]$spanNames.Add([string]$phase['name']) }
    } else {
        foreach ($name in @($PhaseWindows.Keys)) { [void]$spanNames.Add([string]$name) }
    }
    $spanSamples = [System.Collections.Generic.List[object]]::new()
    foreach ($sample in $okSamples) {
        $utc = [long]$sample['utc_ms']
        $inSpan = $false
        if ($spanNames.Count -eq 0) {
            $inSpan = $true
        } else {
            foreach ($name in $spanNames) {
                if ($spanNames.Count -gt 1 -and $name -eq 'reload_baseline') { continue }
                $window = $null
                if ($null -ne $TerminalPhases) { $window = Get-PhaseWindowByName -Phases $TerminalPhases -Name $name }
                if ($null -eq $window -and $null -ne $PhaseWindows -and $PhaseWindows.Contains($name)) { $window = $PhaseWindows[$name] }
                if ($null -ne $window -and $null -ne $window['start_utc_ms'] -and $null -ne $window['end_utc_ms'] -and $utc -ge [long]$window['start_utc_ms'] -and $utc -le [long]$window['end_utc_ms']) {
                    $inSpan = $true
                }
            }
        }
        if ($inSpan) { [void]$spanSamples.Add($sample) }
    }
    if ($spanSamples.Count -lt 2) {
        [void]$anomalies.Add('not enough samples inside the measured span for a first-vs-last comparison; using the whole run')
        $spanSamples = $okSamples
        $summary['windows']['label'] = 'whole-run'
    } else {
        $summary['windows']['label'] = ($spanNames -join '+')
    }
    if ($spanSamples.Count -ge 2) {
        $spanFirst = $spanSamples[0]
        $spanLast = $spanSamples[$spanSamples.Count - 1]
        $spanSeconds = ([double]$spanLast['utc_ms'] - [double]$spanFirst['utc_ms']) / 1000.0
        $windowSeconds = [Math]::Min(10.0, $spanSeconds / 2.0)
        if ($windowSeconds -le 0) { $windowSeconds = 0.001 }
        $firstWindow = [System.Collections.Generic.List[object]]::new()
        $lastWindow = [System.Collections.Generic.List[object]]::new()
        $spanStart = [double]$spanFirst['utc_ms']
        $spanEnd = [double]$spanLast['utc_ms']
        foreach ($sample in $spanSamples) {
            $utc = [double]$sample['utc_ms']
            if ($utc -le $spanStart + $windowSeconds * 1000.0) { [void]$firstWindow.Add($sample) }
            if ($utc -ge $spanEnd - $windowSeconds * 1000.0) { [void]$lastWindow.Add($sample) }
        }
        $summary['windows']['span_seconds'] = $spanSeconds
        $summary['windows']['first_seconds'] = $windowSeconds
        $summary['windows']['last_seconds'] = $windowSeconds
        $summary['windows']['first'] = Get-SampleWindowStats -Samples $firstWindow
        $summary['windows']['last'] = Get-SampleWindowStats -Samples $lastWindow
        $firstStats = $summary['windows']['first']
        $lastStats = $summary['windows']['last']
        if ($null -ne $firstStats['private_mean_bytes'] -and $null -ne $lastStats['private_mean_bytes']) {
            $summary['windows']['private_delta_bytes'] = [double]$lastStats['private_mean_bytes'] - [double]$firstStats['private_mean_bytes']
        }
        if ($null -ne $firstStats['working_set_mean_bytes'] -and $null -ne $lastStats['working_set_mean_bytes']) {
            $summary['windows']['working_set_delta_bytes'] = [double]$lastStats['working_set_mean_bytes'] - [double]$firstStats['working_set_mean_bytes']
        }
        $summary['windows']['cpu_rate_first'] = $firstStats['cpu_rate_cores']
        $summary['windows']['cpu_rate_last'] = $lastStats['cpu_rate_cores']
    }

    # Per-phase resource aggregation over the terminal phase windows.
    # NOTE: PowerShell variable names are case-insensitive, so these locals
    # must not be named like the parameters ($PhaseWindows / $TerminalPhases).
    $phaseWindowList = @($TerminalPhases)
    if ($phaseWindowList.Count -eq 0 -and $null -ne $PhaseWindows) {
        foreach ($name in @($PhaseWindows.Keys)) {
            $window = $PhaseWindows[$name]
            $phaseWindowList += , [ordered]@{ name = [string]$name; start_utc_ms = $window['start_utc_ms']; end_utc_ms = $window['end_utc_ms'] }
        }
    }
    foreach ($phase in $phaseWindowList) {
        $name = [string]$phase['name']
        if ($null -eq $phase['start_utc_ms'] -or $null -eq $phase['end_utc_ms']) { continue }
        $selected = [System.Collections.Generic.List[object]]::new()
        foreach ($sample in $okSamples) {
            $utc = [long]$sample['utc_ms']
            if ($utc -ge [long]$phase['start_utc_ms'] -and $utc -le [long]$phase['end_utc_ms']) { [void]$selected.Add($sample) }
        }
        $stats = Get-SampleWindowStats -Samples $selected
        if (-not $summary['phases'].Contains($name)) {
            $summary['phases'][$name] = [ordered]@{
                samples = 0
                start_utc_ms = [long]$phase['start_utc_ms']
                end_utc_ms = [long]$phase['end_utc_ms']
                private_mean_bytes = $null
                working_set_mean_bytes = $null
                working_set_peak_bytes = $null
                cpu_core_seconds = $null
                average_used_cores = $null
                wall_seconds = $null
            }
        }
        $bucket = $summary['phases'][$name]
        $bucket['samples'] = [int]$bucket['samples'] + $selected.Count
        if ($stats['samples'] -gt 0) {
            if ($null -ne $stats['private_mean_bytes']) { $bucket['private_mean_bytes'] = $stats['private_mean_bytes'] }
            if ($null -ne $stats['working_set_mean_bytes']) { $bucket['working_set_mean_bytes'] = $stats['working_set_mean_bytes'] }
            if ($null -ne $stats['working_set_peak_bytes']) { $bucket['working_set_peak_bytes'] = $stats['working_set_peak_bytes'] }
            if ($null -ne $stats['cpu_delta_seconds']) { $bucket['cpu_core_seconds'] = $stats['cpu_delta_seconds'] }
            if ($null -ne $stats['cpu_rate_cores']) { $bucket['average_used_cores'] = $stats['cpu_rate_cores'] }
            if ($null -ne $stats['span_seconds']) { $bucket['wall_seconds'] = $stats['span_seconds'] }
        }
    }
    return $summary
}

function Get-SampleWindowStats {
    param([object]$Samples)
    $stats = [ordered]@{
        samples = 0
        private_mean_bytes = $null
        working_set_mean_bytes = $null
        working_set_peak_bytes = $null
        cpu_delta_seconds = $null
        cpu_rate_cores = $null
        span_seconds = $null
    }
    $list = @($Samples)
    $stats['samples'] = $list.Count
    if ($list.Count -eq 0) { return $stats }
    $privateSum = [double]0
    $privateCount = 0
    $wsSum = [double]0
    $wsCount = 0
    $wsPeak = $null
    foreach ($sample in $list) {
        if ($null -ne $sample['private_bytes']) {
            $privateSum = $privateSum + [double]$sample['private_bytes']
            $privateCount++
        }
        if ($null -ne $sample['working_set_bytes']) {
            $wsSum = $wsSum + [double]$sample['working_set_bytes']
            $wsCount++
            if ($null -eq $wsPeak -or [long]$sample['working_set_bytes'] -gt [long]$wsPeak) { $wsPeak = $sample['working_set_bytes'] }
        }
    }
    if ($privateCount -gt 0) { $stats['private_mean_bytes'] = $privateSum / $privateCount }
    if ($wsCount -gt 0) {
        $stats['working_set_mean_bytes'] = $wsSum / $wsCount
        $stats['working_set_peak_bytes'] = $wsPeak
    }
    if ($list.Count -ge 2) {
        $first = $list[0]
        $last = $list[$list.Count - 1]
        $span = ([double]$last['utc_ms'] - [double]$first['utc_ms']) / 1000.0
        $stats['span_seconds'] = $span
        if ($null -ne $first['cpu_seconds'] -and $null -ne $last['cpu_seconds'] -and $span -gt 0) {
            $delta = [double]$last['cpu_seconds'] - [double]$first['cpu_seconds']
            $stats['cpu_delta_seconds'] = $delta
            $stats['cpu_rate_cores'] = $delta / $span
        }
    }
    return $stats
}

function New-ResourceCsvText {
    param([object]$Samples)
    $lines = [System.Collections.Generic.List[object]]::new()
    [void]$lines.Add('utc_ms,elapsed_s,phases,ok,private_bytes,working_set_bytes,cpu_seconds,handle_count,gdi,user,gdi_error,user_error,error')
    foreach ($sample in @($Samples)) {
        $cells = @(
            [string]$sample['utc_ms'],
            (Format-Number $sample['elapsed_s'] 3),
            (Format-CsvText ([string]$sample['phases'])),
            ([string]$sample['ok']).ToLowerInvariant(),
            (Format-Number $sample['private_bytes'] 0),
            (Format-Number $sample['working_set_bytes'] 0),
            (Format-Number $sample['cpu_seconds'] 3),
            (Format-Number $sample['handle_count'] 0),
            (Format-Number $sample['gdi'] 0),
            (Format-Number $sample['user'] 0),
            (Format-Number $sample['gdi_error'] 0),
            (Format-Number $sample['user_error'] 0),
            (Format-CsvText ([string]$sample['error']))
        )
        [void]$lines.Add(($cells -join ','))
    }
    return ($lines -join "`n")
}

# ---------------------------------------------------------------------------
# Case execution
# ---------------------------------------------------------------------------

function New-BenchCase {
    param([string]$Name, [int]$InstanceCount, [string]$Mode, [object]$Seconds, [object]$Reloads, [bool]$Quick, [bool]$Wander = $false)
    return [ordered]@{
        name = $Name
        instance_count = $InstanceCount
        mode = $Mode
        seconds = $Seconds
        reloads = $Reloads
        quick = $Quick
        wander = $Wander
        interference = @()
    }
}

function Get-BenchCases {
    param([bool]$QuickMode, [bool]$Wander = $false)
    if ($QuickMode) {
        return @((New-BenchCase -Name 'quick-4-pixel' -InstanceCount 4 -Mode 'pixel' -Seconds 1 -Reloads $null -Quick $true -Wander $Wander))
    }
    $cases = [System.Collections.Generic.List[object]]::new()
    foreach ($count in @(4, 16, 32, 64)) {
        foreach ($mode in @('pixel', 'interactive')) {
            [void]$cases.Add((New-BenchCase -Name ('c' + $count + '-' + $mode) -InstanceCount $count -Mode $mode -Seconds $script:DefaultBenchSeconds -Reloads $null -Quick $false -Wander $Wander))
        }
    }
    [void]$cases.Add((New-BenchCase -Name 'sustained-64-pixel' -InstanceCount 64 -Mode 'pixel' -Seconds 300 -Reloads $null -Quick $false -Wander $Wander))
    [void]$cases.Add((New-BenchCase -Name 'reload-4-pixel' -InstanceCount 4 -Mode 'pixel' -Seconds $null -Reloads $script:DefaultBenchReloads -Quick $false -Wander $Wander))
    return @($cases)
}

function Get-CaseTimeoutSeconds {
    param([object]$Case)
    if ($null -ne $Case['seconds']) {
        return ($script:StartupAllowanceSeconds + $script:WarmupSeconds + [double]$Case['seconds'] + $script:PostRunSlackSeconds)
    }
    # The reload case keeps its planned budget bounded at 180 seconds.
    return ([Math]::Min(180.0, $script:StartupAllowanceSeconds + $script:WarmupSeconds + $script:ReloadBaselineSeconds + [double]$Case['reloads'] * $script:MinSecondsPerSuccess + $script:SettleSeconds + $script:PostRunSlackSeconds))
}

function Get-CaseArguments {
    param([object]$Case)
    $arguments = [System.Collections.Generic.List[object]]::new()
    [void]$arguments.Add('--bench')
    [void]$arguments.Add([string]$Case['instance_count'])
    [void]$arguments.Add('--bench-mode')
    [void]$arguments.Add([string]$Case['mode'])
    if ($Case['wander']) {
        [void]$arguments.Add('--bench-wander')
    }
    if ($null -ne $Case['seconds']) {
        [void]$arguments.Add('--bench-seconds')
        [void]$arguments.Add([string]$Case['seconds'])
    }
    if ($null -ne $Case['reloads']) {
        [void]$arguments.Add('--bench-reloads')
        [void]$arguments.Add([string]$Case['reloads'])
    }
    # Pin the bounded scene explicitly: ordinary launch now spans the primary
    # monitor, while the benchmark workload must keep the fixed 960x640 scene.
    [void]$arguments.Add('--overlay-size')
    [void]$arguments.Add('960x640')
    [void]$arguments.Add('--exit-after')
    [void]$arguments.Add([string]$script:DefaultExitAfter)
    return @($arguments)
}

function Invoke-BenchCase {
    param([object]$Case, [string]$RunDir, [string]$ExecutablePath)
    $reasons = [System.Collections.Generic.List[object]]::new()
    $notes = [System.Collections.Generic.List[object]]::new()
    $caseDir = Join-Path $RunDir ([string]$Case['name'])
    [void][System.IO.Directory]::CreateDirectory($caseDir)
    $stdoutPath = Join-Path $caseDir 'stdout.log'
    $stderrPath = Join-Path $caseDir 'stderr.log'

    $arguments = Get-CaseArguments -Case $Case
    $timeoutSeconds = Get-CaseTimeoutSeconds -Case $Case
    $startedUtcMs = Time-UtcMs
    $wall = [System.Diagnostics.Stopwatch]::StartNew()

    $process = $null
    $nativeHandle = [IntPtr]::Zero
    $childStartTicks = [long]0
    $stdoutText = ''
    $stderrText = ''
    $exitCode = $null
    $timedOut = $false
    $killed = $false
    $closedGracefully = $false
    $identityError = $null
    $samples = [System.Collections.Generic.List[object]]::new()
    $droppedAtExit = 0
    $guiReadiness = [ordered]@{ ready = $false; checks = 0; elapsed_s = $null; timeout_seconds = $script:GuiReadyTimeoutSeconds }
    $interference = [System.Collections.Generic.List[object]]::new()
    $interferenceWarned = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)

    try {
        $psi = [System.Diagnostics.ProcessStartInfo]::new()
        $psi.FileName = $ExecutablePath
        foreach ($argument in $arguments) { [void]$psi.ArgumentList.Add([string]$argument) }
        $psi.WorkingDirectory = $project
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.CreateNoWindow = $true
        $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
        $psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
        $psi.Environment['RUST_LOG'] = 'warn'
        $process = [System.Diagnostics.Process]::new()
        $process.StartInfo = $psi
        [void]$process.Start()
        $nativeHandle = $process.Handle
        $childStartTicks = $process.StartTime.Ticks
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()

        # Timeout accounting includes startup: the case stopwatch starts before
        # Start(), so the external budget covers process creation as well.
        $nextSampleAt = 0.0
        while ($true) {
            $exited = $false
            try { $exited = $process.HasExited } catch { $exited = $true }
            if ($exited) { break }
            $now = $wall.Elapsed.TotalSeconds
            if ($now -ge $timeoutSeconds) {
                $timedOut = $true
                Add-Reason $reasons ('external timeout of ' + (Format-Number $timeoutSeconds 0) + 's reached before the child exited')
                break
            }
            if ($now -ge $nextSampleAt) {
                $identity = Test-ProcessIdentity -Process $process -StartTicks $childStartTicks -NativeHandle $nativeHandle -ExpectedPath $ExecutablePath
                if (-not $identity['ok']) {
                    $exitedDuringCheck = $false
                    try { $exitedDuringCheck = $process.HasExited } catch { }
                    if ($exitedDuringCheck) { break }
                    $identityError = [string]$identity['error']
                    Add-Reason $reasons ('child identity check failed: ' + $identityError)
                    break
                }
                if (-not $guiReadiness['ready']) {
                    $process.Refresh()
                    if ($process.HasExited) { break }
                    $guiReadiness['checks']++
                    $readinessState = Get-GuiReadinessState -WindowHandle $process.MainWindowHandle -ElapsedSeconds $wall.Elapsed.TotalSeconds
                    if ($readinessState -eq 'timed_out') {
                        Add-Reason $reasons ('child GUI readiness timed out after ' + $script:GuiReadyTimeoutSeconds + 's')
                        break
                    }
                    if ($readinessState -eq 'ready') {
                        $guiReadiness['ready'] = $true
                        $guiReadiness['elapsed_s'] = $wall.Elapsed.TotalSeconds
                        Add-Note $notes ('child GUI ready after ' + $guiReadiness['checks'] + ' check(s) at ' + (Format-Number $guiReadiness['elapsed_s']) + 's; resource sampling begins here')
                    }
                }
                if ($guiReadiness['ready']) {
                    $sample = Get-ChildMetrics -Process $process -NativeHandle $nativeHandle -ElapsedSeconds $wall.Elapsed.TotalSeconds
                    if ($null -eq $sample) { $droppedAtExit++ } else { [void]$samples.Add($sample) }
                }
                # Interference watch: any other Delicium process seen while a
                # case runs is reported, flagged in the case result and echoed
                # on the summary line; it is never touched.
                foreach ($observed in @((Find-PreexistingDeliciumProcesses -TargetPath $ExecutablePath)['conflicts'])) {
                    if ([int]$observed['id'] -eq $process.Id) { continue }
                    $key = [string]$observed['id'] + '|' + [string]$observed['name']
                    if ($interferenceWarned.Add($key)) {
                        [void]$interference.Add([ordered]@{ utc_ms = Time-UtcMs; id = $observed['id']; name = $observed['name']; path = $observed['path']; window_title = $observed['window_title'] })
                    }
                }
                if ($interference.Count -gt 0) {
                    Add-Reason $reasons 'another Delicium instance appeared; stopping the benchmark without touching it'
                    break
                }
                $nextSampleAt = [Math]::Floor($now) + 1.0
            }
            Start-Sleep -Milliseconds 100
        }

        if (-not $timedOut -and $null -eq $identityError -and $interference.Count -eq 0 -and $guiReadiness['ready']) {
            try {
                [void]$process.WaitForExit(30000)
                if ($process.HasExited) { $exitCode = $process.ExitCode }
            } catch {
                Add-Reason $reasons ('exit code query failed: ' + $_.Exception.Message)
            }
            $waitAll = [System.Threading.Tasks.Task]::WaitAll([System.Threading.Tasks.Task[]]@($stdoutTask, $stderrTask), 30000)
            if (-not $waitAll) { Add-Reason $reasons 'stdout/stderr stream readers did not finish within 30s' }
            if ($stdoutTask.IsCompleted) { $stdoutText = [string]$stdoutTask.Result } else { Add-Reason $reasons 'stdout stream incomplete' }
            if ($stderrTask.IsCompleted) { $stderrText = [string]$stderrTask.Result } else { Add-Reason $reasons 'stderr stream incomplete' }
        }
    } catch {
        Add-Reason $reasons ('case execution error: ' + $_.Exception.Message)
    } finally {
        if ($null -ne $process) {
            $stillRunning = $false
            try { $stillRunning = -not $process.HasExited } catch { $stillRunning = $false }
            if ($stillRunning) {
                $ticksNow = $null
                try { $ticksNow = $process.StartTime.Ticks } catch { $ticksNow = $null }
                if ($null -ne $ticksNow -and $ticksNow -eq $childStartTicks) {
                    # Graceful close of the held own child first; Kill only when
                    # the original process stays alive. Never Stop-Process -Id.
                    try {
                        $closeRequested = $process.CloseMainWindow()
                        if ($process.WaitForExit(5000)) {
                            if ($closeRequested) { $closedGracefully = $true }
                        } else {
                            $killed = $true
                            $process.Kill()
                            [void]$process.WaitForExit(5000)
                        }
                    } catch {
                        Add-Reason $reasons ('child cleanup error: ' + $_.Exception.Message)
                    }
                } else {
                    $exitedDuringCheck = $false
                    try { $exitedDuringCheck = $process.HasExited } catch { }
                    if (-not $exitedDuringCheck) { Add-Reason $reasons 'child identity changed before cleanup; the process was left untouched' }
                }
            }
            if ($timedOut -or $null -ne $identityError -or $interference.Count -gt 0 -or -not $guiReadiness['ready']) {
                # Preserve whatever the child flushed before it was stopped.
                try {
                    [void]$process.WaitForExit(5000)
                    if ($process.HasExited) { $exitCode = $process.ExitCode }
                    $readers = @(@($stdoutTask, $stderrTask) | Where-Object { $null -ne $_ })
                    if ($readers.Count -gt 0 -and -not [System.Threading.Tasks.Task]::WaitAll([System.Threading.Tasks.Task[]]$readers, 5000)) {
                        Add-Reason $reasons 'stopped child output readers did not finish within 5s'
                    }
                    if ($null -ne $stdoutTask -and $stdoutTask.IsCompleted -and -not $stdoutTask.IsFaulted) { $stdoutText = [string]$stdoutTask.Result }
                    if ($null -ne $stderrTask -and $stderrTask.IsCompleted -and -not $stderrTask.IsFaulted) { $stderrText = [string]$stderrTask.Result }
                } catch { Add-Reason $reasons ('stopped child output preservation failed: ' + $_.Exception.Message) }
            }
        }
    }
    $wallSeconds = [Math]::Round($wall.Elapsed.TotalSeconds, 3)

    try { Write-NewUtf8File -Path $stdoutPath -Text $stdoutText } catch { Add-Reason $reasons ('stdout artifact error: ' + $_.Exception.Message) }
    try { Write-NewUtf8File -Path $stderrPath -Text $stderrText } catch { Add-Reason $reasons ('stderr artifact error: ' + $_.Exception.Message) }

    $result = New-CaseResultSkeleton -Case $Case
    $result['arguments'] = ($arguments -join ' ')
    $result['started_utc_ms'] = $startedUtcMs
    $result['finished_utc_ms'] = Time-UtcMs
    $result['wall_seconds'] = $wallSeconds
    $result['timeout_seconds'] = $timeoutSeconds
    $result['timed_out'] = $timedOut
    $result['killed'] = $killed
    $result['closed_gracefully'] = $closedGracefully
    $result['identity_error'] = $identityError
    $result['gui_readiness'] = $guiReadiness
    if (-not $guiReadiness['ready']) {
        Add-Reason $reasons 'child GUI never became ready; no resource samples were collected'
    }
    $result['exit_code'] = $exitCode
    $result['interference'] = @($interference)
    if ($interference.Count -gt 0) {
        Add-Note $notes ($interference.Count + ' other Delicium process(es) observed during the case; they were not touched, but the numbers may include interference')
    }
    $result['artifacts']['dir'] = $caseDir
    $result['artifacts']['stdout'] = $stdoutPath
    $result['artifacts']['stderr'] = $stderrPath
    $result['artifacts']['result_json'] = Join-Path $caseDir 'result.json'
    $result['artifacts']['resources_json'] = Join-Path $caseDir 'resources.json'
    $result['artifacts']['resources_csv'] = Join-Path $caseDir 'resources.csv'

    $protocol = $null
    $parsed = $null
    $resource = $null
    try {
        if ($null -ne $exitCode -and $exitCode -ne 0) {
            Add-Reason $reasons ('child exit code ' + $exitCode + ' is not 0')
        }
        if ($timedOut) {
            Add-Reason $reasons 'case did not complete before the external timeout'
        }
        $protocol = Get-BenchProtocol -StdoutText $stdoutText
        foreach ($reason in $protocol['reasons']) { Add-Reason $reasons ([string]$reason) }
        foreach ($note in $protocol['notes']) { Add-Note $notes ([string]$note) }
        $result['protocol']['lines'] = [int]$protocol['lines']
        $result['protocol']['event_lines'] = [int]$protocol['event_lines']
        $result['protocol']['terminal_lines'] = [int]$protocol['terminal_lines']
        $result['protocol']['other_lines'] = [int]$protocol['other_lines']
        $result['protocol']['phase_events'] = $protocol['phase_events'].Count
        $result['protocol']['snapshots'] = $protocol['snapshots'].Count
        $result['protocol']['reload_events'] = $protocol['reload_events'].Count
        $result['protocol']['failure_events'] = $protocol['failure_events'].Count
        $result['protocol']['event_parse_errors'] = [int]$protocol['event_parse_errors']
        $result['protocol']['terminal_json_error'] = [bool]$protocol['terminal_json_error']
        $result['protocol']['bom_stripped'] = [bool]$protocol['bom_stripped']
        $result['terminal'] = $protocol['terminal']
        if ($null -ne $protocol['terminal']) {
            $parsed = Read-BenchResult -Terminal $protocol['terminal'] -Reasons $reasons -Notes $notes
            $result['parsed'] = $parsed
        }
        if ($null -ne $protocol['terminal_raw']) {
            $result['terminal_raw'] = [string]$protocol['terminal_raw']
        }
        foreach ($event in @($protocol['failure_events'])) {
            [void]$result['failure_events'].Add($event)
        }
        Test-BenchCaseContract -Case $Case -Parsed $parsed -Protocol $protocol -Reasons $reasons -Notes $notes

        # Annotate the harness samples with the child phase windows after the
        # run, using the terminal phases first and protocol events as fallback.
        $annotationPhases = @()
        if ($null -ne $parsed) { $annotationPhases = @($parsed['phases']) }
        foreach ($sample in $samples) {
            $sample['phases'] = Get-PhaseNamesAt -Phases $annotationPhases -Windows $protocol['phase_windows'] -UtcMs ([long]$sample['utc_ms'])
        }
        $resource = Measure-ResourceSummary -Samples $samples -PhaseWindows $protocol['phase_windows'] -TerminalPhases $annotationPhases
        $resource['samples_dropped_at_exit'] = $droppedAtExit
        $resource['gui_readiness'] = $guiReadiness
        if ([int]$resource['samples_failed'] -gt 0) {
            Add-Reason $reasons ('resource sampling failed on ' + $resource['samples_failed'] + ' sample(s)')
        }
        if ([int]$resource['gdi']['missing_samples'] -gt 0) {
            Add-Reason $reasons ('GetGuiResources(GDI) failed on ' + $resource['gdi']['missing_samples'] + ' sample(s), last error ' + $resource['gdi']['last_error'])
        }
        if ([int]$resource['user']['missing_samples'] -gt 0) {
            Add-Reason $reasons ('GetGuiResources(USER) failed on ' + $resource['user']['missing_samples'] + ' sample(s), last error ' + $resource['user']['last_error'])
        }
        $result['resource'] = $resource
    } catch {
        Add-Reason $reasons ('result analysis error: ' + $_.Exception.Message)
    }

    $uniqueReasons = Get-UniqueStrings -List $reasons
    $uniqueNotes = Get-UniqueStrings -List $notes
    if ($uniqueReasons.Count -gt 40) {
        $total = $uniqueReasons.Count
        $uniqueReasons = @($uniqueReasons[0..39]) + @('... and ' + ($total - 40) + ' more reasons (see the per-case streams)')
    }
    $result['reasons'] = $uniqueReasons
    $result['notes'] = $uniqueNotes
    $result['passed'] = ($uniqueReasons.Count -eq 0)

    $resourcesJson = [ordered]@{
        schema = 'delicium.bench.resources'
        schema_version = 1
        case = [string]$Case['name']
        instance_count = $Case['instance_count']
        mode = $Case['mode']
        count = $samples.Count
        samples = @($samples)
        summary = $resource
    }
    try {
        Write-NewUtf8File -Path $result['artifacts']['resources_json'] -Text (ConvertTo-Json -InputObject $resourcesJson -Depth 12)
        Write-NewUtf8File -Path $result['artifacts']['resources_csv'] -Text (New-ResourceCsvText -Samples $samples)
    } catch {
        Add-Reason $reasons ('resource artifact error: ' + $_.Exception.Message)
        $result['reasons'] = Get-UniqueStrings -List $reasons
        $result['passed'] = $false
    }
    try {
        $resultJson = [ordered]@{
            schema = 'delicium.bench.case'
            schema_version = 1
            case = [string]$Case['name']
            instance_count = $Case['instance_count']
            mode = $Case['mode']
            seconds = $Case['seconds']
            reloads = $Case['reloads']
            quick = $Case['quick']
            wander = [bool]$Case['wander']
            passed = $result['passed']
            reasons = $result['reasons']
            notes = $result['notes']
            harness = [ordered]@{
                arguments = $result['arguments']
                timeout_seconds = $result['timeout_seconds']
                exit_after_seconds = $script:DefaultExitAfter
                started_utc_ms = $result['started_utc_ms']
                finished_utc_ms = $result['finished_utc_ms']
                wall_seconds = $result['wall_seconds']
                timed_out = $result['timed_out']
                killed = $result['killed']
                closed_gracefully = $result['closed_gracefully']
                identity_error = $result['identity_error']
                gui_readiness = $result['gui_readiness']
                exit_code = $result['exit_code']
                interference = @($result['interference'])
                executable = $ExecutablePath
                rust_log = 'warn'
            }
            protocol = $result['protocol']
            terminal = $result['terminal']
            terminal_raw = $result['terminal_raw']
            failure_events = @($result['failure_events'])
            parsed = $result['parsed']
            resource_summary = $result['resource']
            artifacts = [ordered]@{
                stdout = $result['artifacts']['stdout']
                stderr = $result['artifacts']['stderr']
                resources_json = $result['artifacts']['resources_json']
                resources_csv = $result['artifacts']['resources_csv']
            }
        }
        Write-NewUtf8File -Path $result['artifacts']['result_json'] -Text (ConvertTo-Json -InputObject $resultJson -Depth 14)
    } catch {
        Add-Reason $reasons ('result artifact error: ' + $_.Exception.Message)
        $result['reasons'] = Get-UniqueStrings -List $reasons
        $result['passed'] = $false
    }
    return $result
}

function Get-PhaseNamesAt {
    param([object]$Phases, [object]$Windows, [long]$UtcMs)
    $names = [System.Collections.Generic.List[string]]::new()
    foreach ($phase in @($Phases)) {
        if ($null -ne $phase['start_utc_ms'] -and $null -ne $phase['end_utc_ms'] -and $UtcMs -ge [long]$phase['start_utc_ms'] -and $UtcMs -le [long]$phase['end_utc_ms']) {
            [void]$names.Add([string]$phase['name'])
        }
    }
    if ($names.Count -eq 0 -and $null -ne $Windows) {
        foreach ($name in @($Windows.Keys)) {
            $window = $Windows[$name]
            if ($null -ne $window['start_utc_ms'] -and $null -ne $window['end_utc_ms'] -and $UtcMs -ge [long]$window['start_utc_ms'] -and $UtcMs -le [long]$window['end_utc_ms']) {
                [void]$names.Add([string]$name)
            }
        }
    }
    return ($names -join ';')
}

function New-CaseResultSkeleton {
    param([object]$Case)
    return [ordered]@{
        name = $Case['name']
        instance_count = $Case['instance_count']
        mode = $Case['mode']
        seconds = $Case['seconds']
        reloads = $Case['reloads']
        quick = $Case['quick']
        wander = $Case['wander']
        arguments = $null
        started_utc_ms = $null
        finished_utc_ms = $null
        wall_seconds = $null
        timeout_seconds = $null
        timed_out = $false
        killed = $false
        closed_gracefully = $false
        identity_error = $null
        exit_code = $null
        interference = @()
        passed = $false
        reasons = @()
        notes = @()
        protocol = [ordered]@{
            lines = 0
            event_lines = 0
            terminal_lines = 0
            other_lines = 0
            phase_events = 0
            snapshots = 0
            reload_events = 0
            failure_events = 0
            event_parse_errors = 0
            terminal_json_error = $false
            bom_stripped = $false
        }
        terminal = $null
        terminal_raw = $null
        failure_events = [System.Collections.Generic.List[object]]::new()
        parsed = [ordered]@{
            config = [ordered]@{ device_type = $null; control_physical_size = $null }
            phases = @()
            measured_seconds = $null
            scene = [ordered]@{}
            simulation = [ordered]@{}
            regions = [ordered]@{}
            stages = [ordered]@{}
            reloads = [ordered]@{}
            sampler = [ordered]@{}
            gpu_execution_ms = $null
        }
        resource = [ordered]@{
            samples_attempted = 0
            samples_ok = 0
            samples_failed = 0
            samples_dropped_at_exit = 0
            private = [ordered]@{ start_bytes = $null; end_bytes = $null; peak_bytes = $null; peak_utc_ms = $null }
            working_set = [ordered]@{ start_bytes = $null; end_bytes = $null; peak_bytes = $null; peak_utc_ms = $null }
            handles = [ordered]@{ start = $null; end = $null; peak = $null }
            gdi = [ordered]@{ last = $null; peak = $null; missing_samples = 0; last_error = $null }
            user = [ordered]@{ last = $null; peak = $null; missing_samples = 0; last_error = $null }
            cpu = [ordered]@{ core_seconds = $null; wall_seconds = $null; average_used_cores = $null; logical_core_percent = $null; samples_with_cpu = 0 }
            windows = [ordered]@{ label = $null }
            phases = [ordered]@{}
            anomalies = @()
        }
        artifacts = [ordered]@{
            dir = $null
            stdout = $null
            stderr = $null
            result_json = $null
            resources_json = $null
            resources_csv = $null
        }
    }
}

# ---------------------------------------------------------------------------
# Summary artifacts
# ---------------------------------------------------------------------------

function Get-CaseSummaryCells {
    param([object]$CaseResult, [string[]]$PhaseNames)
    $row = [ordered]@{}
    $row['case'] = $CaseResult['name']
    $row['passed'] = ([string]$CaseResult['passed']).ToLowerInvariant()
    $row['instance_count'] = $CaseResult['instance_count']
    $row['mode'] = $CaseResult['mode']
    $row['wander'] = ([string][bool]$CaseResult['wander']).ToLowerInvariant()
    $row['seconds'] = Format-Number $CaseResult['seconds'] 0
    $row['reloads'] = Format-Number $CaseResult['reloads'] 0
    $row['exit_code'] = Format-Number $CaseResult['exit_code'] 0
    $row['timed_out'] = ([string]$CaseResult['timed_out']).ToLowerInvariant()
    $row['wall_seconds'] = Format-Number $CaseResult['wall_seconds'] 3
    $row['reason_count'] = @($CaseResult['reasons']).Count
    $row['reasons_text'] = (@($CaseResult['reasons']) -join '; ')
    $row['interference'] = @($CaseResult['interference']).Count
    $parsed = $CaseResult['parsed']
    $row['completion'] = ''
    $row['build_profile'] = ''
    $row['measured_seconds'] = ''
    if ($null -ne $parsed) {
        if ($null -ne $parsed['completion']) { $row['completion'] = $parsed['completion'] }
        if ($null -ne $parsed['config'] -and $null -ne $parsed['config']['build_profile']) { $row['build_profile'] = $parsed['config']['build_profile'] }
        $row['measured_seconds'] = Format-Number $parsed['measured_seconds'] 3
        $scene = $parsed['scene']
        if ($null -ne $scene) {
            $row['attempts'] = Format-Number $scene['attempts'] 0
            $row['submitted'] = Format-Number $scene['submitted'] 0
            $row['skipped'] = Format-Number $scene['skipped'] 0
            $row['scene_errors'] = Format-Number $scene['errors'] 0
            $row['submitted_per_second'] = Format-Number $scene['submitted_per_second'] 2
        }
        $simulation = $parsed['simulation']
        if ($null -ne $simulation) {
            $row['steps'] = Format-Number $simulation['steps'] 0
            $row['dropped_steps'] = Format-Number $simulation['dropped_steps'] 0
        }
        $regions = $parsed['regions']
        if ($null -ne $regions) {
            $row['regions_rebuilt'] = Format-Number $regions['rebuilt'] 0
            $row['regions_reused'] = Format-Number $regions['reused'] 0
            $row['regions_failed'] = Format-Number $regions['failed'] 0
            $row['polygon_max'] = Format-Number $regions['polygon_max'] 0
        }
        $reloads = $parsed['reloads']
        if ($null -ne $reloads) {
            $row['reloads_requested'] = Format-Number $reloads['requested'] 0
            $row['reloads_successful'] = Format-Number $reloads['successful'] 0
            $row['reloads_render_confirmed'] = Format-Number $reloads['render_confirmed'] 0
            $row['reloads_expected'] = Format-Number $reloads['expected'] 0
        }
        $stages = $parsed['stages']
        foreach ($name in $script:StageNames) {
            $stage = $null
            if ($null -ne $stages) { $stage = $stages[$name] }
            $row[$name + '_count'] = ''
            $row[$name + '_p50_ms'] = ''
            $row[$name + '_p95_ms'] = ''
            $row[$name + '_p99_ms'] = ''
            $row[$name + '_max_ms'] = ''
            if ($null -ne $stage) {
                $row[$name + '_count'] = Format-Number $stage['count'] 0
                $row[$name + '_p50_ms'] = Format-Number $stage['p50_ms'] 3
                $row[$name + '_p95_ms'] = Format-Number $stage['p95_ms'] 3
                $row[$name + '_p99_ms'] = Format-Number $stage['p99_ms'] 3
                $row[$name + '_max_ms'] = Format-Number $stage['max_ms'] 3
            }
        }
    }
    if (-not $row.Contains('attempts')) {
        $row['attempts'] = ''; $row['submitted'] = ''; $row['skipped'] = ''; $row['scene_errors'] = ''; $row['submitted_per_second'] = ''
        $row['steps'] = ''; $row['dropped_steps'] = ''
        $row['regions_rebuilt'] = ''; $row['regions_reused'] = ''; $row['regions_failed'] = ''; $row['polygon_max'] = ''
        $row['reloads_requested'] = ''; $row['reloads_successful'] = ''; $row['reloads_render_confirmed'] = ''; $row['reloads_expected'] = ''
    }
    if (-not $row.Contains('advance_count')) {
        # A case without a parsed terminal still needs the same column set so
        # the CSV header stays consistent across all rows.
        foreach ($stageName in $script:StageNames) {
            $row[$stageName + '_count'] = ''
            $row[$stageName + '_p50_ms'] = ''
            $row[$stageName + '_p95_ms'] = ''
            $row[$stageName + '_p99_ms'] = ''
            $row[$stageName + '_max_ms'] = ''
        }
    }
    $resource = $CaseResult['resource']
    $row['samples_attempted'] = Format-Number $resource['samples_attempted'] 0
    $row['samples_ok'] = Format-Number $resource['samples_ok'] 0
    $row['samples_failed'] = Format-Number $resource['samples_failed'] 0
    $row['samples_dropped_at_exit'] = Format-Number $resource['samples_dropped_at_exit'] 0
    $row['private_start_mib'] = Format-Mib $resource['private']['start_bytes']
    $row['private_end_mib'] = Format-Mib $resource['private']['end_bytes']
    $row['private_peak_mib'] = Format-Mib $resource['private']['peak_bytes']
    $row['ws_start_mib'] = Format-Mib $resource['working_set']['start_bytes']
    $row['ws_end_mib'] = Format-Mib $resource['working_set']['end_bytes']
    $row['ws_peak_mib'] = Format-Mib $resource['working_set']['peak_bytes']
    $row['window_label'] = ''
    $row['private_first_window_mib'] = ''
    $row['private_last_window_mib'] = ''
    $row['private_window_delta_mib'] = ''
    $row['ws_first_window_mib'] = ''
    $row['ws_last_window_mib'] = ''
    $row['ws_window_delta_mib'] = ''
    $windows = $resource['windows']
    if ($null -ne $windows) {
        if ($null -ne $windows['label']) { $row['window_label'] = [string]$windows['label'] }
        if ($null -ne $windows['first'] -and $null -ne $windows['last']) {
            $row['private_first_window_mib'] = Format-Mib $windows['first']['private_mean_bytes']
            $row['private_last_window_mib'] = Format-Mib $windows['last']['private_mean_bytes']
            $row['ws_first_window_mib'] = Format-Mib $windows['first']['working_set_mean_bytes']
            $row['ws_last_window_mib'] = Format-Mib $windows['last']['working_set_mean_bytes']
        }
        $row['private_window_delta_mib'] = Format-Mib $windows['private_delta_bytes']
        $row['ws_window_delta_mib'] = Format-Mib $windows['working_set_delta_bytes']
    }
    $cpu = $resource['cpu']
    $row['cpu_core_seconds'] = Format-Number $cpu['core_seconds'] 3
    $row['cpu_wall_seconds'] = Format-Number $cpu['wall_seconds'] 3
    $row['average_used_cores'] = Format-Number $cpu['average_used_cores'] 3
    $row['logical_core_percent'] = Format-Number $cpu['logical_core_percent'] 2
    $row['cpu_samples'] = Format-Number $cpu['samples_with_cpu'] 0
    $row['handles_end'] = Format-Number $resource['handles']['end'] 0
    $row['gdi_last'] = Format-Number $resource['gdi']['last'] 0
    $row['gdi_error_samples'] = Format-Number $resource['gdi']['missing_samples'] 0
    $row['user_last'] = Format-Number $resource['user']['last'] 0
    $row['user_error_samples'] = Format-Number $resource['user']['missing_samples'] 0
    $row['asset_observation'] = Get-AssetObservation -CaseResult $CaseResult
    $row['gpu_execution_ms'] = 'null'
    if ($null -ne $parsed -and $null -ne $parsed['gpu_execution_ms']) { $row['gpu_execution_ms'] = [string]$parsed['gpu_execution_ms'] }
    $row['sprite_instances_last'] = ''
    $row['skeleton_instances_last'] = ''
    foreach ($phaseName in $PhaseNames) {
        $prefix = 'phase_' + (($phaseName -replace '[^A-Za-z0-9]', '_')) + '_'
        $bucket = $null
        if ($null -ne $resource['phases'] -and $resource['phases'].Contains($phaseName)) { $bucket = $resource['phases'][$phaseName] }
        $row[$prefix + 'samples'] = Format-Number (Get-BucketValue -Bucket $bucket -Name 'samples') 0
        $row[$prefix + 'private_mean_mib'] = Format-Mib (Get-BucketValue -Bucket $bucket -Name 'private_mean_bytes')
        $row[$prefix + 'ws_mean_mib'] = Format-Mib (Get-BucketValue -Bucket $bucket -Name 'working_set_mean_bytes')
        $row[$prefix + 'cpu_core_seconds'] = Format-Number (Get-BucketValue -Bucket $bucket -Name 'cpu_core_seconds') 3
        $row[$prefix + 'cores'] = Format-Number (Get-BucketValue -Bucket $bucket -Name 'average_used_cores') 3
    }
    return $row
}

function Get-AssetObservation {
    param([object]$CaseResult)
    $protocol = $CaseResult['protocol']
    $parsed = $CaseResult['parsed']
    $parts = [System.Collections.Generic.List[object]]::new()
    if ($null -ne $protocol -and [int]$protocol['snapshots'] -gt 0) {
        [void]$parts.Add([string]$protocol['snapshots'] + ' snapshots')
    }
    if ($null -ne $parsed) {
        $sampler = $parsed['sampler']
        if ($null -ne $sampler -and $null -ne $sampler['allocated_bytes']) {
            [void]$parts.Add('sampler allocated ' + (Format-Mib $sampler['allocated_bytes'] 2) + ' MiB')
        }
    }
    if ($null -ne $parsed -and $null -ne $parsed['reloads']) {
        $reloads = $parsed['reloads']
        if ($null -ne $reloads['expected']) {
            [void]$parts.Add('reload assets expected ' + $reloads['expected'])
        }
    }
    return ($parts -join '; ')
}

function Get-SummaryPhaseNames {
    param([object[]]$CaseResults)
    $names = [System.Collections.Generic.List[string]]::new()
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($caseResult in $CaseResults) {
        $resource = $caseResult['resource']
        if ($null -eq $resource) { continue }
        $phases = $resource['phases']
        if ($null -eq $phases) { continue }
        foreach ($name in @($phases.Keys)) {
            if ($seen.Add([string]$name)) { [void]$names.Add([string]$name) }
        }
    }
    return @($names)
}

function New-SummaryCsvText {
    param([object[]]$CaseResults)
    $phaseNames = Get-SummaryPhaseNames -CaseResults $CaseResults
    $rows = [System.Collections.Generic.List[object]]::new()
    foreach ($caseResult in $CaseResults) {
        [void]$rows.Add((Get-CaseSummaryCells -CaseResult $caseResult -PhaseNames $phaseNames))
    }
    return (($rows | ConvertTo-Csv -NoTypeInformation) -join "`n")
}

function New-SummaryMarkdown {
    param([object[]]$CaseResults, [object]$RunInfo)
    $lines = [System.Collections.Generic.List[object]]::new()
    [void]$lines.Add('# Delicium benchmark summary')
    [void]$lines.Add('')
    [void]$lines.Add('- Run id: ' + [string]$RunInfo['run_id'])
    [void]$lines.Add('- Started: ' + [string]$RunInfo['started_utc'])
    [void]$lines.Add('- Finished: ' + [string]$RunInfo['finished_utc'])
    [void]$lines.Add('- Mode: ' + $(if ($RunInfo['quick']) { 'quick wiring run (-Quick)' } else { 'full baseline' }) + $(if ($null -ne $RunInfo['plan'] -and [bool]$RunInfo['plan']['wander']) { ' with autonomous wandering (--bench-wander)' } else { '' }))
    [void]$lines.Add('- Executable: `' + [string]$RunInfo['executable']['path'] + '` (' + [string]$RunInfo['executable']['length'] + ' bytes, last write ' + [string]$RunInfo['executable']['last_write_utc'] + ')')
    [void]$lines.Add('- Host: ' + [string]$RunInfo['host']['os'] + ', ' + [string]$RunInfo['host']['logical_cores'] + ' logical cores, PowerShell ' + [string]$RunInfo['host']['powershell'])
    [void]$lines.Add('- Host recheck: ' + [string]$RunInfo['host_recheck']['os'] + ', ' + [string]$RunInfo['host_recheck']['logical_cores'] + ' logical cores, PowerShell ' + [string]$RunInfo['host_recheck']['powershell'])
    [void]$lines.Add('- Cases: ' + @($CaseResults).Count + ' (' + [string]$RunInfo['passed_count'] + ' passed, ' + [string]$RunInfo['failed_count'] + ' failed)')
    [void]$lines.Add('')
    [void]$lines.Add('## Cases')
    [void]$lines.Add('')
    $caseRows = @()
    foreach ($caseResult in $CaseResults) {
        $parsed = $caseResult['parsed']
        $scene = $null
        if ($null -ne $parsed) { $scene = $parsed['scene'] }
        $caseRows += , @(
            $caseResult['name'],
            $caseResult['instance_count'],
            $caseResult['mode'],
            $(if ($null -ne $caseResult['seconds']) { 'measure ' + [string]$caseResult['seconds'] + 's' } else { 'reload ' + [string]$caseResult['reloads'] }),
            $(if ($caseResult['passed']) { 'PASS' } else { 'FAIL' }),
            $(if ($null -ne $parsed) { [string]$parsed['completion'] } else { '' }),
            (Format-Number $parsed['measured_seconds'] 3),
            $(if ($null -ne $scene) { Format-Number $scene['submitted'] 0 } else { '' }),
            $(if ($null -ne $scene) { Format-Number $scene['submitted_per_second'] 2 } else { '' }),
            (Format-Number $caseResult['resource']['cpu']['average_used_cores'] 2),
            (Format-Mib $caseResult['resource']['private']['start_bytes']),
            (Format-Mib $caseResult['resource']['private']['end_bytes']),
            (Format-Mib $caseResult['resource']['working_set']['peak_bytes']),
            (Format-Number $caseResult['wall_seconds'] 1),
            @($caseResult['reasons']).Count,
            @($caseResult['interference']).Count
        )
    }
    [void]$lines.Add((Format-MarkdownTable -Headers @('case', 'instances', 'mode', 'plan', 'result', 'completion', 'measured_s', 'submitted', 'submitted_per_s', 'avg_cores', 'private_start_MiB', 'private_end_MiB', 'ws_peak_MiB', 'wall_s', 'reasons', 'interference') -Rows $caseRows))
    [void]$lines.Add('')
    [void]$lines.Add('submitted_per_second counts scene submit/present requests, not displayed frames.')
    [void]$lines.Add('')
    [void]$lines.Add('## Reported configuration')
    [void]$lines.Add('')
    [void]$lines.Add('Actual sizes, per-window scale factors and adapter/surface facts come from the terminal configuration and are required for a valid case. No host DPI scale is assumed.')
    [void]$lines.Add('')
    $configRows = @()
    foreach ($caseResult in $CaseResults) {
        $cfg = $null
        if ($null -ne $caseResult['parsed']) { $cfg = $caseResult['parsed']['config'] }
        $configRows += , @(
            $caseResult['name'],
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'device_type')),
            (Format-SizeValue (Get-ConfigValue -Config $cfg -Name 'requested_logical_size')),
            (Format-SizeValue (Get-ConfigValue -Config $cfg -Name 'logical_size')),
            (Format-SizeValue (Get-ConfigValue -Config $cfg -Name 'physical_size')),
            (Format-SizeValue (Get-ConfigValue -Config $cfg -Name 'control_physical_size')),
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'scale_factor')),
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'control_scale_factor')),
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'adapter')),
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'backend')),
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'present_mode')),
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'alpha_mode')),
            (Format-MarkdownCell (Get-ConfigValue -Config $cfg -Name 'format'))
        )
    }
    [void]$lines.Add((Format-MarkdownTable -Headers @('case', 'device_type', 'requested_logical', 'logical', 'physical', 'control_physical', 'scale_factor', 'control_scale_factor', 'adapter', 'backend', 'present_mode', 'alpha_mode', 'format') -Rows $configRows))
    [void]$lines.Add('')
    [void]$lines.Add('## Stage quantiles')
    [void]$lines.Add('')
    [void]$lines.Add('Quantiles are reported by the child over its own bounded sample set; per-second values are not averaged into them.')
    [void]$lines.Add('')
    $stageRows = @()
    foreach ($name in $script:StageNames) {
        $cells = @($name)
        foreach ($caseResult in $CaseResults) {
            $parsed = $caseResult['parsed']
            $stage = $null
            if ($null -ne $parsed -and $null -ne $parsed['stages']) { $stage = $parsed['stages'][$name] }
            if ($null -ne $stage) {
                $cells += ('p50 ' + (Format-Number $stage['p50_ms'] 3) + ' / p95 ' + (Format-Number $stage['p95_ms'] 3) + ' / p99 ' + (Format-Number $stage['p99_ms'] 3) + ' / max ' + (Format-Number $stage['max_ms'] 3))
            } else {
                $cells += ''
            }
        }
        $stageRows += , $cells
    }
    $stageHeaders = @('stage')
    foreach ($caseResult in $CaseResults) { $stageHeaders += [string]$caseResult['name'] }
    [void]$lines.Add((Format-MarkdownTable -Headers $stageHeaders -Rows $stageRows))
    [void]$lines.Add('')
    [void]$lines.Add('## Resources per case')
    [void]$lines.Add('')
    [void]$lines.Add('Private bytes and working set come from the OS process counters at about 1 Hz. The first-versus-last comparison uses equal windows inside the measured span.')
    [void]$lines.Add('')
    foreach ($caseResult in $CaseResults) {
        $resource = $caseResult['resource']
        $windows = $resource['windows']
        [void]$lines.Add('### ' + [string]$caseResult['name'])
        [void]$lines.Add('')
        [void]$lines.Add('- Samples: ' + $resource['samples_ok'] + ' ok, ' + $resource['samples_failed'] + ' failed, ' + $resource['samples_dropped_at_exit'] + ' dropped at exit')
        [void]$lines.Add('- Private bytes: start ' + (Format-Mib $resource['private']['start_bytes']) + ' MiB, end ' + (Format-Mib $resource['private']['end_bytes']) + ' MiB, peak ' + (Format-Mib $resource['private']['peak_bytes']) + ' MiB')
        [void]$lines.Add('- Working set: start ' + (Format-Mib $resource['working_set']['start_bytes']) + ' MiB, end ' + (Format-Mib $resource['working_set']['end_bytes']) + ' MiB, peak ' + (Format-Mib $resource['working_set']['peak_bytes']) + ' MiB')
        [void]$lines.Add('- Handles: start ' + (Format-Number $resource['handles']['start'] 0) + ', end ' + (Format-Number $resource['handles']['end'] 0) + ', peak ' + (Format-Number $resource['handles']['peak'] 0))
        [void]$lines.Add('- GDI objects: last ' + (Format-Number $resource['gdi']['last'] 0) + ', peak ' + (Format-Number $resource['gdi']['peak'] 0) + ', failed queries ' + $resource['gdi']['missing_samples'] + '; USER objects: last ' + (Format-Number $resource['user']['last'] 0) + ', peak ' + (Format-Number $resource['user']['peak'] 0) + ', failed queries ' + $resource['user']['missing_samples'])
        [void]$lines.Add('- CPU: core-seconds ' + (Format-Number $resource['cpu']['core_seconds'] 3) + ' over ' + (Format-Number $resource['cpu']['wall_seconds'] 3) + 's wall, average used cores ' + (Format-Number $resource['cpu']['average_used_cores'] 3) + ', logical-core-normalized ' + (Format-Number $resource['cpu']['logical_core_percent'] 2) + '%')
        if ($null -ne $windows -and $null -ne $windows['first'] -and $null -ne $windows['last']) {
            [void]$lines.Add('- Stable windows (' + [string]$windows['label'] + ', ' + (Format-Number $windows['first_seconds'] 1) + 's each): private ' + (Format-Mib $windows['first']['private_mean_bytes']) + ' -> ' + (Format-Mib $windows['last']['private_mean_bytes']) + ' MiB (delta ' + (Format-Mib $windows['private_delta_bytes']) + ' MiB); working set ' + (Format-Mib $windows['first']['working_set_mean_bytes']) + ' -> ' + (Format-Mib $windows['last']['working_set_mean_bytes']) + ' MiB (delta ' + (Format-Mib $windows['working_set_delta_bytes']) + ' MiB)')
        }
        $phaseNames = @($resource['phases'].Keys)
        if ($phaseNames.Count -gt 0) {
            [void]$lines.Add('')
            [void]$lines.Add('| phase | samples | private mean MiB | ws mean MiB | cpu core-seconds | avg cores |')
            [void]$lines.Add('| --- | --- | --- | --- | --- | --- |')
            foreach ($phaseName in $phaseNames) {
                $bucket = $resource['phases'][$phaseName]
                [void]$lines.Add('| ' + (Format-MarkdownCell $phaseName) + ' | ' + $bucket['samples'] + ' | ' + (Format-Mib $bucket['private_mean_bytes']) + ' | ' + (Format-Mib $bucket['working_set_mean_bytes']) + ' | ' + (Format-Number $bucket['cpu_core_seconds'] 3) + ' | ' + (Format-Number $bucket['average_used_cores'] 3) + ' |')
            }
        }
        if (@($resource['anomalies']).Count -gt 0) {
            [void]$lines.Add('')
            [void]$lines.Add('Anomalies:')
            foreach ($anomaly in @($resource['anomalies'])) { [void]$lines.Add('- ' + [string]$anomaly) }
        }
        if (@($caseResult['reasons']).Count -gt 0) {
            [void]$lines.Add('')
            [void]$lines.Add('Reasons:')
            foreach ($reason in @($caseResult['reasons'])) { [void]$lines.Add('- ' + [string]$reason) }
        }
        if (@($caseResult['notes']).Count -gt 0) {
            [void]$lines.Add('')
            [void]$lines.Add('Notes:')
            foreach ($note in @($caseResult['notes'])) { [void]$lines.Add('- ' + [string]$note) }
        }
        [void]$lines.Add('')
    }
    [void]$lines.Add('## Reload detail')
    [void]$lines.Add('')
    $reloadCase = $null
    foreach ($caseResult in $CaseResults) {
        if ($null -ne $caseResult['reloads']) { $reloadCase = $caseResult; break }
    }
    if ($null -eq $reloadCase) {
        [void]$lines.Add('No reload case in this run.')
    } else {
        $parsed = $reloadCase['parsed']
        if ($null -ne $parsed -and $null -ne $parsed['reloads']) {
            $reloads = $parsed['reloads']
            [void]$lines.Add('- Requested ' + $reloads['requested'] + ', successful ' + $reloads['successful'] + ', render-confirmed ' + $reloads['render_confirmed'] + ', expected ' + $reloads['expected'])
        }
        [void]$lines.Add('- reload_complete events: ' + $reloadCase['protocol']['reload_events'])
        if (@($reloadCase['interference']).Count -gt 0) {
            [void]$lines.Add('- Interference: ' + @($reloadCase['interference']).Count + ' other Delicium process(es) observed during the case')
        }
        [void]$lines.Add('- Artifacts: `' + [string]$reloadCase['artifacts']['dir'] + '`')
    }
    [void]$lines.Add('')
    [void]$lines.Add('## Interpretation limits')
    [void]$lines.Add('')
    [void]$lines.Add('- Process CPU uses OS core-seconds; average_used_cores divides the CPU-time delta by elapsed wall time. Stage timings are host-side wall-clock costs; acquire/submit/present may include driver wait or backpressure. Nested stages are not summed into total CPU time.')
    [void]$lines.Add('- measured_seconds is the actual measured phase sum, including reload_baseline + reload + settle for replacements. Event-loop deadline overshoot is recorded; timed windows must still meet the planned minimum and match that sum. Resource sampling starts at the recorded GUI-readiness time, not process creation.')
    [void]$lines.Add('- submitted_per_second is the scene submit/present request rate, not the display frame rate.')
    [void]$lines.Add('- gpu_execution_ms is null: GPU execution time and VRAM usage are not measured, and process memory is not VRAM.')
    [void]$lines.Add('- Allocator, driver and grow-only buffer capacities need not shrink; an increase between windows is not by itself evidence of a leak, and a single run cannot conclude one either way.')
    [void]$lines.Add('- Resource values cover only the child process this harness started; other processes were neither sampled nor stopped. Other Delicium processes seen during a case are reported as interference and never touched; ratios computed by this harness use a constant logical-core count captured before the first launch.')
    [void]$lines.Add('')
    [void]$lines.Add('## Contract assumptions')
    [void]$lines.Add('')
    foreach ($assumption in @($RunInfo['assumptions'])) { [void]$lines.Add('- ' + [string]$assumption) }
    [void]$lines.Add('')
    return ($lines -join "`n")
}

function Write-RunSummary {
    param([object[]]$CaseResults, [object]$RunInfo, [string]$RunDir)
    $runInfo = $RunInfo
    $runInfo['passed_count'] = 0
    $runInfo['failed_count'] = 0
    foreach ($caseResult in $CaseResults) {
        if ($caseResult['passed']) { $runInfo['passed_count'] = [int]$runInfo['passed_count'] + 1 }
        else { $runInfo['failed_count'] = [int]$runInfo['failed_count'] + 1 }
    }
    $caseSummaries = @()
    foreach ($caseResult in $CaseResults) {
        $caseSummaries += , [ordered]@{
            name = $caseResult['name']
            instance_count = $caseResult['instance_count']
            mode = $caseResult['mode']
            seconds = $caseResult['seconds']
            reloads = $caseResult['reloads']
            quick = $caseResult['quick']
            wander = [bool]$caseResult['wander']
            passed = $caseResult['passed']
            reasons = @($caseResult['reasons'])
            notes = @($caseResult['notes'])
            exit_code = $caseResult['exit_code']
            timed_out = $caseResult['timed_out']
            killed = $caseResult['killed']
            identity_error = $caseResult['identity_error']
            wall_seconds = $caseResult['wall_seconds']
            timeout_seconds = $caseResult['timeout_seconds']
            interference = @($caseResult['interference'])
            protocol = $caseResult['protocol']
            completion = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['completion'] } else { $null })
            measured_seconds = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['measured_seconds'] } else { $null })
            scene = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['scene'] } else { $null })
            simulation = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['simulation'] } else { $null })
            regions = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['regions'] } else { $null })
            reloads_detail = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['reloads'] } else { $null })
            sampler = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['sampler'] } else { $null })
            stages = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['stages'] } else { $null })
            gpu_execution_ms = $(if ($null -ne $caseResult['parsed']) { $caseResult['parsed']['gpu_execution_ms'] } else { $null })
            resource = $caseResult['resource']
            artifacts = $caseResult['artifacts']
        }
    }
    $summaryJson = [ordered]@{
        schema = 'delicium.bench.summary'
        schema_version = 1
        run = $runInfo
        cases = $caseSummaries
    }
    Write-NewUtf8File -Path (Join-Path $RunDir 'summary.json') -Text (ConvertTo-Json -InputObject $summaryJson -Depth 16)
    Write-NewUtf8File -Path (Join-Path $RunDir 'summary.csv') -Text (New-SummaryCsvText -CaseResults $CaseResults)
    Write-NewUtf8File -Path (Join-Path $RunDir 'summary.md') -Text (New-SummaryMarkdown -CaseResults $CaseResults -RunInfo $runInfo)
    return $runInfo
}

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

function Find-PreexistingDeliciumProcesses {
    param([string]$TargetPath)
    $conflicts = [System.Collections.Generic.List[object]]::new()
    $targetName = [System.IO.Path]::GetFileNameWithoutExtension($TargetPath)
    $scanned = 0
    foreach ($candidate in @(Get-Process -ErrorAction SilentlyContinue)) {
        if ($null -eq $candidate) { continue }
        $scanned++
        $name = $null
        try { $name = $candidate.Name } catch { }
        $canonicalName = $null -ne $name -and $name -like 'delicium*'
        $targetNameMatch = $null -ne $name -and $name -ieq $targetName
        if (-not $canonicalName -and -not $targetNameMatch) { continue }
        # Detailed queries only for possible conflicts, not every system process.
        $path = $null
        $title = $null
        $startedTicks = $null
        try { $path = $candidate.Path } catch { }
        try { $title = $candidate.MainWindowTitle } catch { }
        try { $startedTicks = $candidate.StartTime.Ticks } catch { }
        $isConflict = $canonicalName
        if ($targetNameMatch -and ($null -eq $path -or $path -ieq $TargetPath)) { $isConflict = $true }
        if ($isConflict) {
            [void]$conflicts.Add([ordered]@{
                id = $candidate.Id
                name = $name
                path = $path
                window_title = $title
                started_ticks = $startedTicks
            })
        }
    }
    return [ordered]@{ scanned = $scanned; conflicts = @($conflicts) }
}

# ---------------------------------------------------------------------------
# ValidateOnly: binding, protocol, contract, and arithmetic self-checks
# ---------------------------------------------------------------------------

function New-BenchFixtureStage {
    param([long]$Count, [double]$Mean = 1.5)
    if ($Count -le 0) {
        return [ordered]@{ count = 0; mean_ms = $null; p50_ms = $null; p95_ms = $null; p99_ms = $null; max_ms = $null }
    }
    return [ordered]@{
        count = $Count
        mean_ms = $Mean
        p50_ms = [Math]::Round($Mean * 0.9, 3)
        p95_ms = [Math]::Round($Mean * 2.0, 3)
        p99_ms = [Math]::Round($Mean * 2.5, 3)
        max_ms = [Math]::Round($Mean * 3.0, 3)
    }
}

function New-BenchFixturePhases {
    param([object]$Seconds, [object]$Reloads, [double]$WarmupSeconds = 10.0, [double]$SettleSeconds = 10.0)
    if ($null -ne $Seconds) {
        $measureStart = 3.0 + $WarmupSeconds
        return @(
            [ordered]@{ phase = 'setup'; start_utc_ms = 1000; end_utc_ms = 4000; start_elapsed_s = 0.0; end_elapsed_s = 3.0 },
            [ordered]@{ phase = 'warmup'; start_utc_ms = 4000; end_utc_ms = (4000 + [long]($WarmupSeconds * 1000)); start_elapsed_s = 3.0; end_elapsed_s = 3.0 + $WarmupSeconds },
            [ordered]@{ phase = 'measure'; start_utc_ms = (4000 + [long]($WarmupSeconds * 1000)); end_utc_ms = (4000 + [long](($WarmupSeconds + [double]$Seconds) * 1000)); start_elapsed_s = 3.0 + $WarmupSeconds; end_elapsed_s = 3.0 + $WarmupSeconds + [double]$Seconds },
            [ordered]@{ phase = 'done'; start_utc_ms = (4000 + [long](($WarmupSeconds + [double]$Seconds) * 1000)); end_utc_ms = (4200 + [long](($WarmupSeconds + [double]$Seconds) * 1000)); start_elapsed_s = 3.0 + $WarmupSeconds + [double]$Seconds; end_elapsed_s = 3.0 + $WarmupSeconds + [double]$Seconds + 0.2 }
        )
    }
    $baselineStart = 4.0 + $WarmupSeconds
    $reloadStart = $baselineStart + 5.0
    $reloadEnd = $reloadStart + 125.0
    $settleEnd = $reloadEnd + $SettleSeconds
    return @(
        [ordered]@{ phase = 'setup'; start_utc_ms = 1000; end_utc_ms = 4000; start_elapsed_s = 0.0; end_elapsed_s = 3.0 },
        [ordered]@{ phase = 'warmup'; start_utc_ms = 4000; end_utc_ms = (4000 + [long]($WarmupSeconds * 1000)); start_elapsed_s = 3.0; end_elapsed_s = 3.0 + $WarmupSeconds },
        [ordered]@{ phase = 'reload_baseline'; start_utc_ms = (4000 + [long]($WarmupSeconds * 1000)); end_utc_ms = (4000 + [long](($WarmupSeconds + 5.0) * 1000)); start_elapsed_s = 3.0 + $WarmupSeconds; end_elapsed_s = 3.0 + $WarmupSeconds + 5.0 },
        [ordered]@{ phase = 'reload'; start_utc_ms = (4000 + [long](($WarmupSeconds + 5.0) * 1000)); end_utc_ms = (4000 + [long](($WarmupSeconds + 5.0 + 125.0) * 1000)); start_elapsed_s = 3.0 + $WarmupSeconds + 5.0; end_elapsed_s = 3.0 + $WarmupSeconds + 5.0 + 125.0 },
        [ordered]@{ phase = 'settle'; start_utc_ms = (4000 + [long](($WarmupSeconds + 5.0 + 125.0) * 1000)); end_utc_ms = (4000 + [long](($WarmupSeconds + 5.0 + 125.0 + $SettleSeconds) * 1000)); start_elapsed_s = 3.0 + $WarmupSeconds + 5.0 + 125.0; end_elapsed_s = 3.0 + $WarmupSeconds + 5.0 + 125.0 + $SettleSeconds }
    )
}

function New-BenchFixtureTerminal {
    param(
        [int]$InstanceCount = 4,
        [string]$Mode = 'pixel',
        [object]$Seconds = 30,
        [object]$Reloads = $null,
        [string]$BuildProfile = 'release',
        [bool]$Wander = $false,
        [object]$Valid = $true,
        [string]$Completion = 'completed',
        [object]$Failure = $null,
        [object]$GpuExecutionMs = $null,
        [object]$Phases = $null
    )
    $phaseList = $Phases
    if ($null -eq $phaseList) { $phaseList = New-BenchFixturePhases -Seconds $Seconds -Reloads $Reloads }
    $measurable = 0.0
    foreach ($phase in @($phaseList)) {
        if ($phase['phase'] -in @('measure', 'reload_baseline', 'reload', 'settle')) {
            $measurable += [double]$phase['end_elapsed_s'] - [double]$phase['start_elapsed_s']
        }
    }
    $terminal = [ordered]@{
        schema_version = 1
        valid = $Valid
        completion = $Completion
        failure = $Failure
        config = [ordered]@{
            instance_count = $InstanceCount
            mode = $Mode
            seconds = $Seconds
            reloads = $Reloads
            build_profile = $BuildProfile
            wander = [bool]$Wander
            quick = $false
            executable = 'C:\fixture\delicium.exe'
            workdir = 'C:\fixture'
            warmup_seconds = 10
            requested_logical_size = @(960, 640)
            logical_size = @(960, 640)
            physical_size = @(1440, 960)
            control_physical_size = @(570, 1020)
            scale_factor = 1.5
            control_scale_factor = 1.5
            adapter = 'fixture hardware'
            backend = 'dx12'
            device_type = 'DiscreteGpu'
            present_mode = 'Fifo'
            alpha_mode = 'PreMultiplied'
            format = 'Bgra8Unorm'
        }
        phases = $phaseList
        measured_seconds = $measurable
        scene = [ordered]@{
            attempts = 1800
            submitted = 1798
            skipped = 2
            errors = 0
            submitted_per_second = $(if ($measurable -gt 0) { 1798.0 / $measurable } else { 0.0 })
            skip_reasons = [ordered]@{ suboptimal = 2 }
        }
        simulation = [ordered]@{ steps = 3600; dropped_steps = 0 }
        regions = [ordered]@{ rebuilt = 900; reused = 900; failed = 0; polygon_max = 7 }
        stages = [ordered]@{
            advance = New-BenchFixtureStage -Count 1798 -Mean 0.12
            render_frame = New-BenchFixtureStage -Count 1798 -Mean 0.35
            hit_geometry = New-BenchFixtureStage -Count 1798 -Mean 0.08
            hit_compare = New-BenchFixtureStage -Count 0
            native_region_build_install = New-BenchFixtureStage -Count 900 -Mean 0.4
            acquire = New-BenchFixtureStage -Count 1798 -Mean 0.05
            encode_upload = New-BenchFixtureStage -Count 1798 -Mean 0.3
            submit = New-BenchFixtureStage -Count 1798 -Mean 0.25
            present = New-BenchFixtureStage -Count 1798 -Mean 0.2
            scene_callback = New-BenchFixtureStage -Count 1798 -Mean 1.2
            control_callback = New-BenchFixtureStage -Count 60 -Mean 0.6
            frame_interval = New-BenchFixtureStage -Count 1797 -Mean 16.6
        }
        reloads = [ordered]@{
            requested = $(if ($null -ne $Reloads) { $Reloads } else { 0 })
            successful = $(if ($null -ne $Reloads) { $Reloads } else { 0 })
            render_confirmed = $(if ($null -ne $Reloads) { $Reloads } else { 0 })
            expected = $Reloads
        }
        sampler = [ordered]@{ scene_capacity = 65536; control_capacity = 65536; allocated_bytes = 2097152; scene_used = 1800; control_used = 60 }
        gpu_execution_ms = $GpuExecutionMs
    }
    $events = [System.Collections.Generic.List[object]]::new()
    foreach ($phase in $phaseList) {
        [void]$events.Add('BENCH_EVENT ' + (ConvertTo-Json -InputObject ([ordered]@{ event = 'phase'; schema_version = 1; phase = $phase['phase']; utc_ms = $phase['start_utc_ms']; elapsed_s = $phase['start_elapsed_s'] }) -Compress -Depth 5))
    }
    return [ordered]@{ terminal = $terminal; events = $events }
}

function New-BenchFixtureStdout {
    param(
        [object]$Fixture,
        [int]$SnapshotCount = 3,
        [object]$SnapshotInstance = $null,
        [object]$SnapshotCpu = 4,
        [object]$SnapshotGpu = 4,
        [object]$SnapshotExpected = 4,
        [switch]$OmitSnapshots,
        [int]$ReloadEventCount = -1,
        [object]$ReloadEventInstance = 0,
        [switch]$OmitCpuAssetsFromSnapshot,
        [switch]$OmitCycleFromReload,
        [switch]$OmitTerminal,
        [switch]$DuplicateTerminal,
        [switch]$CorruptTerminal,
        [string[]]$ExtraLines
    )
    $lines = [System.Collections.Generic.List[object]]::new()
    foreach ($event in @($Fixture['events'])) { [void]$lines.Add([string]$event) }
    $terminal = $Fixture['terminal']
    $config = $terminal['config']
    $instanceCount = [int]$config['instance_count']
    if ($null -eq $SnapshotInstance) { $SnapshotInstance = $instanceCount }
    if (-not $OmitSnapshots) {
        foreach ($index in 1..$SnapshotCount) {
            $snapshot = [ordered]@{
                event = 'snapshot'
                schema_version = 1
                phase = 'warmup'
                utc_ms = (5000 + $index * 1000)
                elapsed_s = (4.0 + $index)
                instance_count = [int]$SnapshotInstance
                sprite_instances = 2
                skeleton_instances = 2
                cpu_assets = [int]$SnapshotCpu
                gpu_assets = [int]$SnapshotGpu
                expected_assets = [int]$SnapshotExpected
                mask_bytes = 4096
                sprite_vertex_capacity_bytes = 1048576
                instance_capacity_bytes = 65536
                palette_capacity_bytes = 4096
                pending = 0
                region_mode = 'pixel'
            }
            if ($OmitCpuAssetsFromSnapshot) { [void]$snapshot.Remove('cpu_assets') }
            [void]$lines.Add('BENCH_EVENT ' + (ConvertTo-Json -InputObject $snapshot -Compress -Depth 5))
        }
    }
    if ($null -ne $config['reloads']) {
        $count = [int]$config['reloads']
        if ($ReloadEventCount -ge 0) { $count = $ReloadEventCount }
        foreach ($request in 1..$count) {
            $reloadPhase = 'reload'
            if ($request -eq $count) { $reloadPhase = 'settle' }
            $reload = [ordered]@{
                event = 'reload_complete'
                schema_version = 1
                # The final completion can legitimately arrive after the
                # transition into settle; the validator must not require a
                # specific completion phase.
                phase = $reloadPhase
                request = ($request + 2)
                instance = [int]$ReloadEventInstance
                asset = (3 + ($request % 2))
                cycle = $request
                utc_ms = (15000 + $request * 1000)
                elapsed_s = (12.0 + $request)
                cpu_assets = 4
                gpu_assets = 4
                expected_assets = 4
            }
            if ($OmitCycleFromReload) { [void]$reload.Remove('cycle') }
            [void]$lines.Add('BENCH_EVENT ' + (ConvertTo-Json -InputObject $reload -Compress -Depth 5))
        }
    }
    if ($null -ne $ExtraLines) {
        foreach ($line in $ExtraLines) { [void]$lines.Add([string]$line) }
    }
    if (-not $OmitTerminal) {
        $json = ConvertTo-Json -InputObject $terminal -Compress -Depth 8
        if ($CorruptTerminal) {
            [void]$lines.Add('BENCH_RESULT {"valid":')
        } else {
            [void]$lines.Add('BENCH_RESULT ' + $json)
            if ($DuplicateTerminal) { [void]$lines.Add('BENCH_RESULT ' + $json) }
        }
    }
    return ($lines -join "`n")
}

function Invoke-FixtureCheck {
    param([string]$Label, [object]$Case, [string]$StdoutText, [string[]]$Expect, [string[]]$Forbid, [switch]$RequireClean)
    $protocol = Get-BenchProtocol -StdoutText $StdoutText
    $reasons = [System.Collections.Generic.List[object]]::new()
    $notes = [System.Collections.Generic.List[object]]::new()
    foreach ($reason in $protocol['reasons']) { Add-Reason $reasons ([string]$reason) }
    $parsed = $null
    if ($null -ne $protocol['terminal']) {
        $parsed = Read-BenchResult -Terminal $protocol['terminal'] -Reasons $reasons -Notes $notes
    }
    Test-BenchCaseContract -Case $Case -Parsed $parsed -Protocol $protocol -Reasons $reasons -Notes $notes
    if ($RequireClean -and @($reasons).Count -gt 0) {
        throw ('fixture check failed: ' + $Label + ' unexpected reasons=[' + (@($reasons) -join '; ') + ']')
    }
    $joined = (@($reasons) -join "`n")
    $missing = [System.Collections.Generic.List[object]]::new()
    foreach ($token in @($Expect)) {
        if (-not $joined.Contains($token)) { [void]$missing.Add($token) }
    }
    $forbidden = [System.Collections.Generic.List[object]]::new()
    foreach ($token in @($Forbid)) {
        if ($joined.Contains($token)) { [void]$forbidden.Add($token) }
    }
    if ($missing.Count -eq 0 -and $forbidden.Count -eq 0) {
        return [ordered]@{ label = $Label; passed = $true; reasons = @($reasons); notes = @($notes); parsed = $parsed; protocol = $protocol }
    }
    $detail = 'missing=[' + ($missing -join '; ') + '] forbidden=[' + ($forbidden -join '; ') + '] reasons=[' + $joined + ']'
    throw ('fixture check failed: ' + $Label + ' ' + $detail)
}

function Invoke-ValidationSelfChecks {
    $script:ValidateChecks = 0

    # 1. Native binding compiles and reports GetGuiResources errors instead of
    #    a fake zero.
    $own = [System.Diagnostics.Process]::GetCurrentProcess()
    $ownGui = [BenchNative]::Gui($own.Handle)
    if ($ownGui.Length -ne 4) { throw 'BenchNative.Gui must return four integers' }
    if ([int]$ownGui[1] -ne 0 -or [int]$ownGui[3] -ne 0) { throw 'BenchNative.Gui failed on the harness process' }
    if ([int]$ownGui[0] -lt 0 -or [int]$ownGui[2] -lt 0) { throw 'BenchNative.Gui returned negative counts' }
    $deadGui = [BenchNative]::Gui([IntPtr]::Zero)
    if ([int]$deadGui[1] -eq 0 -or [int]$deadGui[3] -eq 0) { throw 'BenchNative.Gui must report an error for an invalid handle, not a fake zero' }
    if ([int]$deadGui[0] -ne 0 -or [int]$deadGui[2] -ne 0) { throw 'BenchNative.Gui must not report counts for an invalid handle' }
    $ownPath = [BenchNative]::ImagePath($own.Handle)
    if ([string]::IsNullOrEmpty($ownPath)) { throw 'BenchNative.ImagePath failed on the harness process' }
    Write-Output ('[OK] BenchNative binding compiled; own GDI/USER ' + $ownGui[0] + '/' + $ownGui[2] + ', error propagation verified')
    $script:ValidateChecks++

    foreach ($case in @(
        @([IntPtr]::Zero, 0.0, 'waiting'),
        @([IntPtr]::Zero, 29.99, 'waiting'),
        @([IntPtr]::new(1), 1.0, 'ready'),
        @([IntPtr]::Zero, 30.0, 'timed_out'),
        @([IntPtr]::new(1), 30.0, 'timed_out')
    )) {
        $state = Get-GuiReadinessState -WindowHandle $case[0] -ElapsedSeconds $case[1]
        if ($state -ne $case[2]) { throw ('GUI readiness state mismatch: ' + $state) }
        $script:ValidateChecks++
    }
    Write-Output '[OK] GUI readiness waits for an owned window and enforces its deadline without querying GUI resources'

    # 2. Valid fixture: protocol, contract, and snapshot checks all pass.
    $casePixel = New-BenchCase -Name 'fixture-4-pixel' -InstanceCount 4 -Mode 'pixel' -Seconds 30 -Reloads $null -Quick $false
    $caseReload = New-BenchCase -Name 'fixture-4-reload' -InstanceCount 4 -Mode 'pixel' -Seconds $null -Reloads 100 -Quick $false
    $validFixture = New-BenchFixtureTerminal
    $validCheck = Invoke-FixtureCheck -Label 'valid timed fixture' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $validFixture) -Expect @() -Forbid @() -RequireClean
    $parsed = $validCheck['parsed']
    if ($null -eq $parsed) { throw 'valid fixture produced no parsed terminal' }
    if ($parsed['scene']['attempts'] -ne ($parsed['scene']['submitted'] + $parsed['scene']['skipped'])) { throw 'fixture conservation arithmetic broken' }
    if ($validCheck['protocol']['snapshots'].Count -ne 3) { throw 'snapshot count not parsed' }
    Write-Output '[OK] valid fixture parses and satisfies the case contract'
    $script:ValidateChecks++

    $reloadFixture = New-BenchFixtureTerminal -Seconds $null -Reloads 100
    $reloadCheck = Invoke-FixtureCheck -Label 'valid reload fixture' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $reloadFixture) -Expect @() -Forbid @() -RequireClean
    if ($reloadCheck['protocol']['reload_events'].Count -ne 100) { throw 'reload events not parsed' }
    Write-Output '[OK] valid reload fixture parses with 100 reload_complete events'
    $script:ValidateChecks++

    # 3. Malformed protocol fixtures.
    Invoke-FixtureCheck -Label 'missing terminal' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $validFixture -OmitTerminal) -Expect @('no BENCH_RESULT terminal line') -Forbid @() | Out-Null
    $script:ValidateChecks++
    Invoke-FixtureCheck -Label 'duplicate terminal' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $validFixture -DuplicateTerminal) -Expect @('more than one BENCH_RESULT line') -Forbid @() | Out-Null
    $script:ValidateChecks++
    Invoke-FixtureCheck -Label 'corrupt terminal JSON' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $validFixture -CorruptTerminal) -Expect @('BENCH_RESULT is not valid JSON') -Forbid @() | Out-Null
    $script:ValidateChecks++
    Invoke-FixtureCheck -Label 'malformed event JSON' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $validFixture -ExtraLines @('BENCH_EVENT {"event": ')) -Expect @('malformed BENCH_EVENT JSON') -Forbid @() | Out-Null
    $script:ValidateChecks++
    Invoke-FixtureCheck -Label 'unexpected BENCH line' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $validFixture -ExtraLines @('BENCH_RESULTX stray')) -Expect @('unexpected BENCH line') -Forbid @() | Out-Null
    $script:ValidateChecks++

    # 4. Contract fixture matrix.
    $invalid = New-BenchFixtureTerminal -Valid $false -Completion 'failed'
    Invoke-FixtureCheck -Label 'invalid terminal' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $invalid) -Expect @('valid is not true', 'completion is "failed"') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $withFailure = New-BenchFixtureTerminal -Failure 'scene callback failed'
    Invoke-FixtureCheck -Label 'failure text' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $withFailure) -Expect @('failure is not null: scene callback failed') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $debugProfile = New-BenchFixtureTerminal -BuildProfile 'debug'
    Invoke-FixtureCheck -Label 'debug build profile' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $debugProfile) -Expect @('build_profile "debug" is not "release"') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $gpuSet = New-BenchFixtureTerminal -GpuExecutionMs 3.5
    Invoke-FixtureCheck -Label 'gpu_execution_ms set' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $gpuSet) -Expect @('gpu_execution_ms must be null') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $wrongInstance = New-BenchFixtureTerminal -InstanceCount 8
    Invoke-FixtureCheck -Label 'planned instance mismatch' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $wrongInstance -SnapshotInstance 8 -SnapshotCount 1) -Expect @('config.instance_count 8 does not match the planned 4', 'snapshot instance_count mismatch') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $wrongMode = New-BenchFixtureTerminal -Mode 'interactive'
    Invoke-FixtureCheck -Label 'planned mode mismatch' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $wrongMode) -Expect @('config.mode "interactive" does not match the planned "pixel"') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $wrongSeconds = New-BenchFixtureTerminal -Seconds 29.5
    Invoke-FixtureCheck -Label 'planned seconds mismatch' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $wrongSeconds) -Expect @('measure phase duration', 'config.seconds') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $wanderCase = New-BenchCase -Name 'fixture-4-pixel-wander' -InstanceCount 4 -Mode 'pixel' -Seconds 30 -Reloads $null -Quick $false -Wander $true
    $wrongWander = New-BenchFixtureTerminal -Wander $false
    Invoke-FixtureCheck -Label 'planned wander mismatch' -Case $wanderCase -StdoutText (New-BenchFixtureStdout -Fixture $wrongWander) -Expect @('config.wander False does not match the planned True') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $expectedDefaultArguments = '--bench 4 --bench-mode pixel --bench-seconds 30 --overlay-size 960x640 --exit-after 600'
    $defaultArguments = (@(Get-CaseArguments -Case $casePixel) -join ' ')
    if ($defaultArguments -ne $expectedDefaultArguments) { throw ('default case arguments changed: ' + $defaultArguments) }
    $wanderArguments = (@(Get-CaseArguments -Case $wanderCase) -join ' ')
    if ($wanderArguments -ne '--bench 4 --bench-mode pixel --bench-wander --bench-seconds 30 --overlay-size 960x640 --exit-after 600') { throw ('wander case arguments changed: ' + $wanderArguments) }
    Write-Output '[OK] case arguments are frozen for the stationary workload and carry --bench-wander for the wandering workload'
    $script:ValidateChecks++
    $overshoot = New-BenchFixtureTerminal -Phases (New-BenchFixturePhases -Seconds 30.097 -Reloads $null)
    $overshootCheck = Invoke-FixtureCheck -Label 'deadline overshoot' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $overshoot) -Expect @() -Forbid @() -RequireClean
    if (-not (@($overshootCheck['notes']) -match 'event-loop deadline overshoot')) { throw 'deadline overshoot was not recorded' }
    $script:ValidateChecks++
    $caseSustained = New-BenchCase -Name 'fixture-64-sustained' -InstanceCount 64 -Mode 'pixel' -Seconds 300 -Reloads $null -Quick $false
    $sustainedOvershoot = New-BenchFixtureTerminal -InstanceCount 64 -Seconds 300 -Phases (New-BenchFixturePhases -Seconds 300.097 -Reloads $null)
    Invoke-FixtureCheck -Label 'sustained deadline overshoot' -Case $caseSustained -StdoutText (New-BenchFixtureStdout -Fixture $sustainedOvershoot) -Expect @() -Forbid @() -RequireClean | Out-Null
    $script:ValidateChecks++
    $shortMeasure = New-BenchFixtureTerminal -Phases (New-BenchFixturePhases -Seconds 29.5 -Reloads $null)
    Invoke-FixtureCheck -Label 'short actual measure with correct config' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $shortMeasure) -Expect @('measure phase duration') -Forbid @('config.seconds') | Out-Null
    $script:ValidateChecks++
    $wrongMeasuredSum = New-BenchFixtureTerminal
    $wrongMeasuredSum['terminal']['measured_seconds'] = 30.2
    Invoke-FixtureCheck -Label 'measured phase sum mismatch' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $wrongMeasuredSum) -Expect @('does not match measured phase sum') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $shortWarmup = New-BenchFixtureTerminal -Phases (New-BenchFixturePhases -Seconds 30 -Reloads $null -WarmupSeconds 9.5)
    Invoke-FixtureCheck -Label 'short warmup' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $shortWarmup) -Expect @('warmup phase duration 9.500s is below the required 10s') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $noMeasure = New-BenchFixtureTerminal -Phases (New-BenchFixturePhases -Seconds 30 -Reloads $null)[0, 1, 3]
    Invoke-FixtureCheck -Label 'missing measure phase' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $noMeasure) -Expect @('measure phase missing from phases') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $brokenConservation = New-BenchFixtureTerminal
    $brokenConservation['terminal']['scene']['submitted'] = 1797
    Invoke-FixtureCheck -Label 'broken conservation' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $brokenConservation) -Expect @('scene conservation broken') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $sceneErrors = New-BenchFixtureTerminal
    $sceneErrors['terminal']['scene']['errors'] = 2
    $sceneErrors['terminal']['scene']['attempts'] = 1802
    $sceneErrors['terminal']['scene']['submitted'] = 1800
    $sceneErrors['terminal']['scene']['skipped'] = 2
    Invoke-FixtureCheck -Label 'scene errors' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $sceneErrors) -Expect @('scene.errors is 2') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $regionFailure = New-BenchFixtureTerminal
    $regionFailure['terminal']['regions']['failed'] = 1
    Invoke-FixtureCheck -Label 'region fallback' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $regionFailure) -Expect @('regions.failed is 1') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $samplerOverflow = New-BenchFixtureTerminal
    $samplerOverflow['terminal']['sampler']['scene_used'] = 70000
    Invoke-FixtureCheck -Label 'sampler overflow' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $samplerOverflow) -Expect @('sampler overflow: scene_used 70000') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $stageTimingMissing = New-BenchFixtureTerminal
    $stageTimingMissing['terminal']['stages']['advance']['p95_ms'] = $null
    Invoke-FixtureCheck -Label 'stage timing null with count' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $stageTimingMissing) -Expect @('stages.advance has count 1798 but p95_ms is null') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $stageTimingExtra = New-BenchFixtureTerminal
    $stageTimingExtra['terminal']['stages']['hit_compare']['mean_ms'] = 2.0
    Invoke-FixtureCheck -Label 'stage timing present at count 0' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $stageTimingExtra) -Expect @('stages.hit_compare has count 0 but mean_ms is not null') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $stageMissing = New-BenchFixtureTerminal
    [void]$stageMissing['terminal']['stages'].Remove('frame_interval')
    Invoke-FixtureCheck -Label 'stage key missing' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $stageMissing) -Expect @('stages.frame_interval missing') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $assetMismatch = New-BenchFixtureTerminal
    Invoke-FixtureCheck -Label 'snapshot asset mismatch' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $assetMismatch -SnapshotExpected 4 -SnapshotCpu 3 -SnapshotGpu 3) -Expect @('snapshot asset mismatch in 3 of 3 snapshots') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $snapshotIssue = New-BenchFixtureTerminal
    Invoke-FixtureCheck -Label 'snapshot field missing' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $snapshotIssue -OmitCpuAssetsFromSnapshot) -Expect @('snapshot protocol issues') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $noSnapshots = New-BenchFixtureTerminal
    Invoke-FixtureCheck -Label 'no snapshots' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $noSnapshots -OmitSnapshots) -Expect @('no BENCH_EVENT snapshot was emitted') -Forbid @() | Out-Null
    $script:ValidateChecks++

    # 5. Reload contract fixtures.
    $reloadMismatch = New-BenchFixtureTerminal -Seconds $null -Reloads 100
    $reloadMismatch['terminal']['reloads']['successful'] = 99
    Invoke-FixtureCheck -Label 'reload counter mismatch' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $reloadMismatch -ReloadEventCount 99) -Expect @('reloads.successful is 99, expected 100', 'reload_complete events 99 do not match the planned 100') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $seqFixture = New-BenchFixtureTerminal -Seconds $null -Reloads 100
    $seqStdout = New-BenchFixtureStdout -Fixture $seqFixture -ReloadEventCount 100
    $seqStdout = $seqStdout -replace '"cycle":50,', '"cycle":500,'
    Invoke-FixtureCheck -Label 'reload cycle sequence broken' -Case $caseReload -StdoutText $seqStdout -Expect @('reload cycle numbers are not 1..100') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $reloadMeasured = New-BenchFixtureTerminal -Seconds $null -Reloads 100
    $reloadMeasured['terminal']['measured_seconds'] = 125.0
    Invoke-FixtureCheck -Label 'reload measured phase sum mismatch' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $reloadMeasured) -Expect @('does not match measured phase sum') -Forbid @() | Out-Null
    $script:ValidateChecks++
    foreach ($missing in @($false, $true)) {
        $noMeasured = New-BenchFixtureTerminal -Seconds $null -Reloads 100
        if ($missing) { [void]$noMeasured['terminal'].Remove('measured_seconds') }
        else { $noMeasured['terminal']['measured_seconds'] = $null }
        Invoke-FixtureCheck -Label 'reload measured seconds absent' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $noMeasured) -Expect @('measured_seconds must be a number') -Forbid @() | Out-Null
        $script:ValidateChecks++
    }
    $fastReloadPhases = New-BenchFixturePhases -Seconds $null -Reloads 100
    foreach ($phase in $fastReloadPhases) {
        if ($phase['phase'] -eq 'reload') {
            $phase['end_elapsed_s'] -= 101.0
            $phase['end_utc_ms'] -= 101000
        } elseif ($phase['phase'] -eq 'settle') {
            $phase['start_elapsed_s'] -= 101.0
            $phase['end_elapsed_s'] -= 101.0
            $phase['start_utc_ms'] -= 101000
            $phase['end_utc_ms'] -= 101000
        }
    }
    $fastReload = New-BenchFixtureTerminal -Seconds $null -Reloads 100 -Phases $fastReloadPhases
    Invoke-FixtureCheck -Label 'reload request spacing too short' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $fastReload) -Expect @('below the required request spacing') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $missingBaselinePhases = @(New-BenchFixturePhases -Seconds $null -Reloads 100 | Where-Object { $_['phase'] -ne 'reload_baseline' })
    $missingBaseline = New-BenchFixtureTerminal -Seconds $null -Reloads 100 -Phases $missingBaselinePhases
    Invoke-FixtureCheck -Label 'reload baseline phase missing' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $missingBaseline) -Expect @('reload_baseline phase has no usable time window') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $duplicateMeasuredPhases = @(New-BenchFixturePhases -Seconds 30 -Reloads $null)
    $duplicateMeasuredPhases += $duplicateMeasuredPhases[2]
    $duplicateMeasured = New-BenchFixtureTerminal -Phases $duplicateMeasuredPhases
    Invoke-FixtureCheck -Label 'duplicate measured phase' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $duplicateMeasured) -Expect @('expected exactly one measured phase') -Forbid @() | Out-Null
    $script:ValidateChecks++
    Write-Output '[OK] actual measured phase sums, deadline overshoot, minimum durations and reload spacing are checked'
    $shortSettle = New-BenchFixtureTerminal -Seconds $null -Reloads 100 -Phases (New-BenchFixturePhases -Seconds $null -Reloads 100 -SettleSeconds 8.0)
    Invoke-FixtureCheck -Label 'short settle' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $shortSettle) -Expect @('settle phase duration 8.000s is below the required 10s') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $cycleMissing = New-BenchFixtureTerminal -Seconds $null -Reloads 100
    Invoke-FixtureCheck -Label 'reload event missing cycle' -Case $caseReload -StdoutText (New-BenchFixtureStdout -Fixture $cycleMissing -OmitCycleFromReload) -Expect @('reload_complete protocol issues') -Forbid @() | Out-Null
    $script:ValidateChecks++
    $timedClean = Invoke-FixtureCheck -Label 'timed fixture produces no contract reasons' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture (New-BenchFixtureTerminal -Seconds 30 -Reloads $null)) -Expect @() -Forbid @()
    if (@($timedClean['reasons']).Count -ne 0) {
        throw ('timed fixture produced unexpected reasons: ' + (@($timedClean['reasons']) -join '; '))
    }
    $script:ValidateChecks++

    # 6. Count conservation arithmetic.
    $conservation = New-BenchFixtureTerminal
    $conservation['terminal']['scene']['attempts'] = 1811
    $conservation['terminal']['scene']['submitted'] = 1800
    $conservation['terminal']['scene']['skipped'] = 11
    $conservation['terminal']['scene']['skip_reasons'] = [ordered]@{ suboptimal = 11 }
    $conservationCheck = Invoke-FixtureCheck -Label 'count conservation accepted' -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $conservation) -Expect @() -Forbid @() -RequireClean
    $scene = $conservationCheck['parsed']['scene']
    if ([long]$scene['attempts'] -ne ([long]$scene['submitted'] + [long]$scene['skipped'])) { throw 'conservation arithmetic wrong' }
    Write-Output '[OK] count conservation accepted when attempts = submitted + skipped'
    $script:ValidateChecks++

    # 7. CPU arithmetic from known samples.
    $cpuSamples = [System.Collections.Generic.List[object]]::new()
    [void]$cpuSamples.Add([ordered]@{ utc_ms = 1000; elapsed_s = 0.0; ok = $true; private_bytes = 104857600; working_set_bytes = 125829120; cpu_seconds = 0.5; handle_count = 200; gdi = 10; user = 20; gdi_error = 0; user_error = 0; error = $null })
    [void]$cpuSamples.Add([ordered]@{ utc_ms = 1500; elapsed_s = 0.5; ok = $false; private_bytes = $null; working_set_bytes = $null; cpu_seconds = $null; handle_count = $null; gdi = $null; user = $null; gdi_error = $null; user_error = $null; error = 'query failed' })
    [void]$cpuSamples.Add([ordered]@{ utc_ms = 2000; elapsed_s = 1.0; ok = $true; private_bytes = 115343360; working_set_bytes = 136314880; cpu_seconds = 1.5; handle_count = 210; gdi = 12; user = 22; gdi_error = 0; user_error = 0; error = $null })
    [void]$cpuSamples.Add([ordered]@{ utc_ms = 3000; elapsed_s = 2.0; ok = $true; private_bytes = 116391936; working_set_bytes = 137363456; cpu_seconds = 2.2; handle_count = 212; gdi = 12; user = 22; gdi_error = 0; user_error = 0; error = $null })
    $cpuSummary = Measure-ResourceSummary -Samples $cpuSamples -PhaseWindows ([ordered]@{}) -TerminalPhases @()
    if ([int]$cpuSummary['samples_failed'] -ne 1) { throw 'failed sample was not counted' }
    if ([Math]::Abs([double]$cpuSummary['cpu']['core_seconds'] - 1.7) -gt 0.0001) { throw ('cpu core-seconds wrong: ' + $cpuSummary['cpu']['core_seconds']) }
    if ([Math]::Abs([double]$cpuSummary['cpu']['wall_seconds'] - 2.0) -gt 0.0001) { throw ('cpu wall seconds wrong: ' + $cpuSummary['cpu']['wall_seconds']) }
    if ([Math]::Abs([double]$cpuSummary['cpu']['average_used_cores'] - 0.85) -gt 0.0001) { throw ('average used cores wrong: ' + $cpuSummary['cpu']['average_used_cores']) }
    $expectedPercent = 0.85 / [double]$script:LogicalCores * 100.0
    if ([Math]::Abs([double]$cpuSummary['cpu']['logical_core_percent'] - $expectedPercent) -gt 0.0001) { throw 'logical core percent wrong' }
    if ([long]$cpuSummary['private']['end_bytes'] -ne 116391936) { throw 'private end bytes wrong' }
    if ([long]$cpuSummary['private']['peak_bytes'] -ne 116391936) { throw 'private peak bytes wrong' }
    if ($cpuSummary['gdi']['last'] -ne 12) { throw 'GDI last value wrong' }
    if ($cpuSummary['anomalies'].Count -ne 1) { throw ('expected exactly one anomaly for the failed sample, got ' + $cpuSummary['anomalies'].Count) }
    Write-Output '[OK] CPU core-seconds, wall time, average cores, and normalized percent match the in-memory samples'
    $script:ValidateChecks++

    # 8. GetGuiResources failure semantics in the sampler summary.
    $guiSamples = [System.Collections.Generic.List[object]]::new()
    [void]$guiSamples.Add([ordered]@{ utc_ms = 1000; elapsed_s = 0.0; ok = $true; private_bytes = 1024; working_set_bytes = 2048; cpu_seconds = 0.1; handle_count = 10; gdi = 0; user = 5; gdi_error = 0; user_error = 0; error = $null })
    [void]$guiSamples.Add([ordered]@{ utc_ms = 2000; elapsed_s = 1.0; ok = $true; private_bytes = 1024; working_set_bytes = 2048; cpu_seconds = 0.2; handle_count = 10; gdi = $null; user = 5; gdi_error = 87; user_error = 0; error = $null })
    $guiSummary = Measure-ResourceSummary -Samples $guiSamples -PhaseWindows ([ordered]@{}) -TerminalPhases @()
    if ($guiSummary['gdi']['last'] -ne 0 -or $guiSummary['gdi']['peak'] -ne 0) { throw 'legitimate zero GDI count was not preserved' }
    if ($guiSummary['gdi']['missing_samples'] -ne 1) { throw 'failed GDI query was not counted' }
    if ($guiSummary['gdi']['last_error'] -ne 87) { throw 'GDI error code was not preserved' }
    if ($guiSummary['user']['last'] -ne 5) { throw 'USER count changed unexpectedly' }
    Write-Output '[OK] zero GDI counts are preserved and failed queries keep their error code instead of a fake zero'
    $script:ValidateChecks++

    # 9. Stable-window arithmetic.
    $windowSamples = [System.Collections.Generic.List[object]]::new()
    foreach ($index in 0..11) {
        [void]$windowSamples.Add([ordered]@{
            utc_ms = (10000 + $index * 1000)
            elapsed_s = [double]$index
            ok = $true
            private_bytes = (104857600 + $index * 1048576)
            working_set_bytes = (125829120 + $index * 1048576)
            cpu_seconds = (0.25 * $index)
            handle_count = 200
            gdi = 10
            user = 20
            gdi_error = 0
            user_error = 0
            error = $null
        })
    }
    $phases = @(
        [ordered]@{ name = 'warmup'; start_utc_ms = 9000; end_utc_ms = 10000; start_elapsed_s = 0.0; end_elapsed_s = 1.0 },
        [ordered]@{ name = 'measure'; start_utc_ms = 10000; end_utc_ms = 24000; start_elapsed_s = 1.0; end_elapsed_s = 15.0 }
    )
    $windowSummary = Measure-ResourceSummary -Samples $windowSamples -PhaseWindows ([ordered]@{}) -TerminalPhases $phases
    if ([string]$windowSummary['windows']['label'] -ne 'measure') { throw ('stable window label wrong: ' + $windowSummary['windows']['label']) }
    if ([int]$windowSummary['windows']['first']['samples'] -lt 2 -or [int]$windowSummary['windows']['last']['samples'] -lt 2) { throw 'window sample selection failed' }
    $firstMean = [double]$windowSummary['windows']['first']['private_mean_bytes']
    $lastMean = [double]$windowSummary['windows']['last']['private_mean_bytes']
    # Span 10000..21000 ms with 5.5s windows: the first window covers samples
    # 0..5 (mean base+2.5 MiB), the last covers 6..11 (mean base+8.5 MiB); the
    # 15000 ms sample belongs to the first window only.
    if ([Math]::Abs($firstMean - 107479040.0) -gt 1.0) { throw ('first window mean wrong: ' + $firstMean) }
    if ([Math]::Abs($lastMean - 113770496.0) -gt 1.0) { throw ('last window mean wrong: ' + $lastMean) }
    if ([Math]::Abs([double]$windowSummary['windows']['private_delta_bytes'] - 6291456.0) -gt 1.0) { throw 'private window delta wrong' }
    if (-not $windowSummary['phases'].Contains('measure')) { throw 'per-phase aggregation missing' }
    if ([int]$windowSummary['phases']['measure']['samples'] -ne 12) { throw ('per-phase sample count wrong: ' + $windowSummary['phases']['measure']['samples']) }
    Write-Output '[OK] first-versus-last stable windows and per-phase aggregation match the in-memory samples'
    $script:ValidateChecks++

    # 10. Negative CPU deltas are surfaced, not hidden.
    $resetSamples = [System.Collections.Generic.List[object]]::new()
    [void]$resetSamples.Add([ordered]@{ utc_ms = 1000; elapsed_s = 0.0; ok = $true; private_bytes = 1024; working_set_bytes = 2048; cpu_seconds = 5.0; handle_count = 10; gdi = 1; user = 1; gdi_error = 0; user_error = 0; error = $null })
    [void]$resetSamples.Add([ordered]@{ utc_ms = 2000; elapsed_s = 1.0; ok = $true; private_bytes = 1024; working_set_bytes = 2048; cpu_seconds = 4.0; handle_count = 10; gdi = 1; user = 1; gdi_error = 0; user_error = 0; error = $null })
    $resetSummary = Measure-ResourceSummary -Samples $resetSamples -PhaseWindows ([ordered]@{}) -TerminalPhases @()
    if ($resetSummary['anomalies'].Count -eq 0) { throw 'negative CPU delta was not flagged' }
    if ($null -ne $resetSummary['cpu']['average_used_cores']) { throw 'average used cores must not be derived from a negative delta' }
    Write-Output '[OK] negative CPU deltas are flagged and never converted into core usage'
    $script:ValidateChecks++

    # 11. Optional event metadata stays optional while wrong types are caught.
    $noElapsed = Get-BenchProtocol -StdoutText 'BENCH_EVENT {"event":"phase","schema_version":1,"phase":"warmup","utc_ms":1000}'
    if ($noElapsed['reasons'].Count -ne 0) { throw ('missing phase elapsed_s must not fail: ' + (@($noElapsed['reasons']) -join '; ')) }
    if ($null -ne $noElapsed['phase_events'][0]['elapsed_s']) { throw 'missing phase elapsed_s must stay null' }
    $badElapsed = Get-BenchProtocol -StdoutText 'BENCH_EVENT {"event":"phase","schema_version":1,"phase":"warmup","elapsed_s":"soon"}'
    if ($badElapsed['reasons'].Count -eq 0) { throw 'non-numeric phase elapsed_s must be flagged' }
    $badEventSchema = Get-BenchProtocol -StdoutText 'BENCH_EVENT {"event":"phase","schema_version":2,"phase":"warmup"}'
    if ($badEventSchema['reasons'].Count -eq 0) { throw 'event schema_version 2 must be flagged' }
    Write-Output '[OK] optional phase elapsed_s stays optional while non-numeric values and wrong schema versions are flagged'
    $script:ValidateChecks++

    # 12. Actual configuration facts are required; malformed values fail closed.
    foreach ($name in @('requested_logical_size', 'logical_size', 'physical_size', 'control_physical_size', 'scale_factor', 'control_scale_factor', 'adapter', 'backend', 'device_type', 'present_mode', 'alpha_mode', 'format', 'wander')) {
        $missingMetadata = New-BenchFixtureTerminal
        [void]$missingMetadata['terminal']['config'].Remove($name)
        Invoke-FixtureCheck -Label ('missing config ' + $name) -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $missingMetadata) -Expect @('config.' + $name + ' missing') -Forbid @() | Out-Null
    }
    foreach ($bad in @(
        @{ name = 'device_type'; value = @(1, 2) },
        @{ name = 'physical_size'; value = @(0, 640) },
        @{ name = 'control_physical_size'; value = @(380) },
        @{ name = 'scale_factor'; value = 0 },
        @{ name = 'control_scale_factor'; value = '1.5' },
        @{ name = 'adapter'; value = '' },
        @{ name = 'wander'; value = 'on' }
    )) {
        $badMetadata = New-BenchFixtureTerminal
        $badMetadata['terminal']['config'][$bad.name] = $bad.value
        Invoke-FixtureCheck -Label ('malformed config ' + $bad.name) -Case $casePixel -StdoutText (New-BenchFixtureStdout -Fixture $badMetadata) -Expect @('config.' + $bad.name + ' has an unexpected type') -Forbid @() | Out-Null
    }
    $failureEvent = Get-BenchProtocol -StdoutText 'BENCH_EVENT {"event":"failure","schema_version":1,"phase":"failed","utc_ms":1000,"error":"fixture asset mismatch"}'
    if ($failureEvent['failure_events'][0]['reason'] -ne 'fixture asset mismatch') { throw 'failure event error text was lost' }
    Write-Output '[OK] required configuration, malformed metadata and failure-event error text are checked'
    $script:ValidateChecks++
    # 13. Summary generation with valid and malformed case results.
    $goodCase = New-CaseResultSkeleton -Case $casePixel
    $goodCase['passed'] = $true
    $goodCase['exit_code'] = 0
    $goodCase['wall_seconds'] = 43.5
    $goodCase['timeout_seconds'] = 160
    $goodCase['resource'] = $cpuSummary
    $goodCase['parsed']['completion'] = 'completed'
    $goodCase['parsed']['measured_seconds'] = 30.0
    $goodCase['parsed']['scene'] = [ordered]@{ attempts = 1800; submitted = 1798; skipped = 2; errors = 0; submitted_per_second = 59.9 }
    $goodCase['parsed']['regions'] = [ordered]@{ rebuilt = 1; reused = 1; failed = 0; polygon_max = 3 }
    $goodCase['parsed']['reloads'] = [ordered]@{ requested = 0; successful = 0; render_confirmed = 0; expected = $null }
    $goodCase['parsed']['config'] = (New-BenchFixtureTerminal)['terminal']['config']
    $badCase = New-CaseResultSkeleton -Case $caseReload
    $badCase['passed'] = $false
    $badCase['reasons'] = @('terminal valid is not true')
    $badCase['exit_code'] = 1
    $badCase['resource'] = [ordered]@{
        samples_attempted = 0
        samples_ok = 0
        samples_failed = 0
        samples_dropped_at_exit = 0
        private = [ordered]@{ start_bytes = $null; end_bytes = $null; peak_bytes = $null; peak_utc_ms = $null }
        working_set = [ordered]@{ start_bytes = $null; end_bytes = $null; peak_bytes = $null; peak_utc_ms = $null }
        handles = [ordered]@{ start = $null; end = $null; peak = $null }
        gdi = [ordered]@{ last = $null; peak = $null; missing_samples = 0; last_error = $null }
        user = [ordered]@{ last = $null; peak = $null; missing_samples = 0; last_error = $null }
        cpu = [ordered]@{ core_seconds = $null; wall_seconds = $null; average_used_cores = $null; logical_core_percent = $null; samples_with_cpu = 0 }
        windows = [ordered]@{ label = $null }
        phases = [ordered]@{}
        anomalies = @()
    }
    $csvText = New-SummaryCsvText -CaseResults @($goodCase, $badCase)
    if (-not $csvText.Contains('fixture-4-pixel')) { throw 'summary csv missing the valid case name' }
    if (-not $csvText.Contains('fixture-4-reload')) { throw 'summary csv missing the malformed case name' }
    if (-not $csvText.Contains('terminal valid is not true')) { throw 'summary csv missing the malformed case reason' }
    if (-not $csvText.Contains('59.90')) { throw 'summary csv missing the submission rate' }
    $markdown = New-SummaryMarkdown -CaseResults @($goodCase, $badCase) -RunInfo ([ordered]@{
        run_id = 'validate-only'
        started_utc = 'fixture'
        finished_utc = 'fixture'
        quick = $true
        passed_count = 1
        failed_count = 1
        executable = [ordered]@{ path = 'C:\fixture\delicium.exe'; length = 1; last_write_utc = 'fixture' }
        host = [ordered]@{ os = 'fixture'; logical_cores = $script:LogicalCores; powershell = $PSVersionTable.PSVersion.ToString() }
        host_recheck = [ordered]@{ os = 'fixture'; logical_cores = $script:LogicalCores; powershell = $PSVersionTable.PSVersion.ToString() }
        assumptions = $script:HarnessAssumptions
    })
    if (-not $markdown.Contains('## Interpretation limits')) { throw 'summary markdown missing the interpretation section' }
    if (-not $markdown.Contains('terminal valid is not true')) { throw 'summary markdown missing the failed case reason' }
    if (-not $markdown.Contains('fixture hardware') -or -not $markdown.Contains('1440') -or -not $markdown.Contains('control_scale_factor')) { throw 'summary markdown missing actual configuration facts' }
    Write-Output '[OK] summary json/csv/markdown generation preserves configuration and handles malformed case results'
    $script:ValidateChecks++
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if ($ValidateOnly) {
    if ($Quick) { Write-Output '[WARN] -Quick is ignored while -ValidateOnly runs' }
    if ($Wander) { Write-Output '[WARN] -Wander is ignored while -ValidateOnly runs' }
    Invoke-ValidationSelfChecks
    Write-Output ('VALIDATE_ONLY pass=true checks=' + $script:ValidateChecks)
    exit 0
}

# Reject UNC/network or device executable paths before any path lookup, then
# reject explicit escape from the repository tree. There is no output-path
# override; artifacts always stay under the repository-local .artifacts.
if ($Executable -match '^\s*[\\/]{2}') {
    Write-Output '[PREFLIGHT] [FAIL] network (UNC) or device executable paths are not accepted by this harness'
    exit 1
}
$executablePath = [System.IO.Path]::GetFullPath($Executable)
$projectSeparator = [System.IO.Path]::DirectorySeparatorChar
if (-not ($executablePath -eq $project -or $executablePath.StartsWith($project + $projectSeparator, [System.StringComparison]::OrdinalIgnoreCase))) {
    Write-Output ('[PREFLIGHT] [FAIL] executable path escapes the repository: ' + $executablePath)
    exit 1
}
$runIdPrefix = $(if ($Wander) { 'windows-bench-wander-' } else { 'windows-bench-' })
$runId = $runIdPrefix + [DateTime]::Now.ToString('yyyyMMdd-HHmmss-ffff') + '-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
$runDir = Join-Path $project ('.artifacts/' + $runId)
if ([System.IO.Directory]::Exists($runDir)) { throw ('Refusing to reuse an existing artifact path: ' + $runDir) }
[void][System.IO.Directory]::CreateDirectory($runDir)
Write-Output ('[PREFLIGHT] artifacts: ' + $runDir)

$preflight = [ordered]@{
    scanned_at_utc_ms = Time-UtcMs
    executable = [ordered]@{
        path = $executablePath
        exists = [System.IO.File]::Exists($executablePath)
        length = $null
        last_write_utc = $null
    }
    process_scan = $null
    exe_dir_recheck = $null
    note = 'No input injection, no screenshots, no network. Do not launch the application while this script runs.'
}
$exeSizeBefore = $null
$exeDirWriteBefore = $null
if ($preflight['executable']['exists']) {
    $exeSizeBefore = (Get-Item -LiteralPath $executablePath).Length
    $exeDirWriteBefore = (Get-Item -LiteralPath $executablePath).Directory.LastWriteTimeUtc.Ticks
    $fileInfo = [System.IO.FileInfo]::new($executablePath)
    $preflight['executable']['length'] = $fileInfo.Length
    $preflight['executable']['last_write_utc'] = $fileInfo.LastWriteTimeUtc.ToString('o')
    Write-Output ('[PREFLIGHT] executable: ' + $executablePath + ' (' + $fileInfo.Length + ' bytes, last write ' + $preflight['executable']['last_write_utc'] + ')')
} else {
    Write-Output ('[PREFLIGHT] [FAIL] executable not found: ' + $executablePath)
    Write-Output '[PREFLIGHT] build the release binary first: cargo build --release'
}

$scan = Find-PreexistingDeliciumProcesses -TargetPath $executablePath
$preflight['process_scan'] = $scan
$conflicts = @($scan['conflicts'])
if ($conflicts.Count -gt 0) {
    Write-Output ('[PREFLIGHT] [FAIL] ' + $conflicts.Count + ' preexisting Delicium process(es) found; the benchmark will not start and no process was stopped')
    foreach ($conflict in $conflicts) {
        Write-Output ('[PREFLIGHT]   pid=' + $conflict['id'] + ' name=' + [string]$conflict['name'] + ' path=' + [string]$conflict['path'] + ' window="' + [string]$conflict['window_title'] + '"')
    }
} elseif (-not $preflight['executable']['exists']) {
    Write-Output '[PREFLIGHT] [FAIL] missing executable; nothing to run'
} else {
    Write-Output ('[PREFLIGHT] no preexisting Delicium process found (scanned ' + $scan['scanned'] + ' processes)')
}

$canRun = ($conflicts.Count -eq 0) -and $preflight['executable']['exists']
if ($canRun) {
    # Catch a build that rewrote the executable between preflight and launch.
    $exeSizeAfter = (Get-Item -LiteralPath $executablePath).Length
    $exeDirWriteAfter = (Get-Item -LiteralPath $executablePath).Directory.LastWriteTimeUtc.Ticks
    $preflight['exe_dir_recheck'] = [ordered]@{
        size_before = $exeSizeBefore
        size_after = $exeSizeAfter
        directory_write_before_ticks = $exeDirWriteBefore
        directory_write_after_ticks = $exeDirWriteAfter
        stable = (($exeSizeBefore -eq $exeSizeAfter) -and ($exeDirWriteBefore -eq $exeDirWriteAfter))
    }
    if (-not $preflight['exe_dir_recheck']['stable']) {
        Write-Output '[PREFLIGHT] [FAIL] the target directory changed between preflight and launch (an active build?); not starting'
        $canRun = $false
    } else {
        Write-Output '[PREFLIGHT] target exe and directory stable for launch'
    }
}
Write-NewUtf8File -Path (Join-Path $runDir 'preflight.json') -Text (ConvertTo-Json -InputObject $preflight -Depth 8)

if (-not $canRun) {
    Write-Output ''
    Write-Output ('WINDOWS_BENCH_RESULT pass=false cases=0 artifacts=' + $runDir)
    exit 1
}

$runStartedUtc = [DateTime]::UtcNow.ToString('o')
$cases = @(Get-BenchCases -QuickMode ([bool]$Quick) -Wander ([bool]$Wander))
$caseCount = $cases.Count
$caseResults = [System.Collections.Generic.List[object]]::new()
$caseIndex = 0
foreach ($case in $cases) {
    $caseIndex++
    $label = [string]$case['name']
    Write-Output ('[CASE ' + $caseIndex + '/' + $caseCount + ' ' + $label + '] ' + ('--bench ' + $case['instance_count'] + ' --bench-mode ' + $case['mode'] + $(if ($case['wander']) { ' --bench-wander' } else { '' }) + $(if ($null -ne $case['seconds']) { ' --bench-seconds ' + $case['seconds'] } else { '' }) + $(if ($null -ne $case['reloads']) { ' --bench-reloads ' + $case['reloads'] } else { '' })))
    $result = Invoke-BenchCase -Case $case -RunDir $runDir -ExecutablePath $executablePath
    [void]$caseResults.Add($result)
    $summaryLine = '[CASE ' + $caseIndex + '/' + $caseCount + ' ' + $label + '] exit=' + (Format-Number $result['exit_code'] 0) + ' wall=' + (Format-Number $result['wall_seconds'] 1) + 's samples=' + $result['resource']['samples_ok'] + '/' + $result['resource']['samples_attempted'] + ' result=' + $(if ($result['passed']) { 'PASS' } else { 'FAIL' })
    Write-Output $summaryLine
    if (-not $result['passed']) {
        foreach ($reason in @($result['reasons'])) { Write-Output ('[CASE ' + $label + '] reason: ' + $reason) }
    }
    foreach ($note in @($result['notes'])) { Write-Output ('[CASE ' + $label + '] note: ' + $note) }
    if (@($result['interference']).Count -gt 0) {
        Write-Output ('[CASE ' + $label + '] [FAIL] interference: ' + @($result['interference']).Count + ' other Delicium process(es) observed; foreign processes untouched, remaining cases stopped')
        break
    }
}

$runInfo = [ordered]@{
    run_id = $runId
    script_version = $script:ScriptVersion
    quick = [bool]$Quick
    started_utc = $runStartedUtc
    finished_utc = $null
    executable = [ordered]@{
        path = $executablePath
        length = $preflight['executable']['length']
        last_write_utc = $preflight['executable']['last_write_utc']
    }
    host = [ordered]@{
        os = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
        logical_cores = $script:LogicalCores
        powershell = $PSVersionTable.PSVersion.ToString()
    }
    host_recheck = [ordered]@{
        os = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
        logical_cores = [System.Environment]::ProcessorCount
        powershell = $PSVersionTable.PSVersion.ToString()
    }
    plan = [ordered]@{
        wander = [bool]$Wander
        warmup_seconds = $script:WarmupSeconds
        reload_baseline_seconds = $script:ReloadBaselineSeconds
        settle_seconds = $script:SettleSeconds
        min_seconds_per_success = $script:MinSecondsPerSuccess
        default_seconds = $script:DefaultBenchSeconds
        default_reloads = $script:DefaultBenchReloads
        exit_after_seconds = $script:DefaultExitAfter
        modes = @('pixel', 'interactive')
        instance_counts = @(4, 16, 32, 64)
    }
    assumptions = $script:HarnessAssumptions
    artifacts = [ordered]@{ dir = $runDir; preflight = (Join-Path $runDir 'preflight.json') }
}
$runInfo['finished_utc'] = [DateTime]::UtcNow.ToString('o')
$runInfo = Write-RunSummary -CaseResults @($caseResults) -RunInfo $runInfo -RunDir $runDir

$failed = [int]$runInfo['failed_count']
$total = [int]$runInfo['passed_count'] + $failed
Write-Output ''
Write-Output ('Cases: ' + $total + ' total, ' + $runInfo['passed_count'] + ' passed, ' + $failed + ' failed')
Write-Output ('WINDOWS_BENCH_RESULT pass=' + $(if ($failed -eq 0) { 'true' } else { 'false' }) + ' cases=' + $total + ' artifacts=' + $runDir)
Write-Output ('summary: ' + (Join-Path $runDir 'summary.md'))
$finalExitCode = 1
if ($failed -eq 0) { $finalExitCode = 0 }
exit $finalExitCode
