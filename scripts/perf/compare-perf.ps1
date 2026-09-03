param(
    [Parameter(Mandatory = $true)][string]$BeforeDir,
    [Parameter(Mandatory = $true)][string]$AfterDir,
    [double]$RegressionTolerancePct = 5.0,
    [switch]$FailOnRegression
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
Import-Module (Join-Path $repositoryRoot "scripts/perf/perf-owner-closure.psm1") -Force

function Read-JsonLines {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (!(Test-Path $Path)) {
        return @()
    }

    $rows = @()
    Get-Content -Path $Path | ForEach-Object {
        $line = $_.Trim()
        if ([string]::IsNullOrWhiteSpace($line)) {
            return
        }
        if ($line.StartsWith("[")) {
            $parsedArray = $line | ConvertFrom-Json
            foreach ($item in $parsedArray) {
                $rows += $item
            }
        } else {
            $rows += ($line | ConvertFrom-Json)
        }
    }
    return $rows
}

function Strict-ByKey {
    param(
        [array]$Rows = @(),
        [Parameter(Mandatory = $true)][string]$Key
    )
    $map = @{}
    if ($null -eq $Rows) {
        return $map
    }
    foreach ($row in $Rows) {
        if (!($row.PSObject.Properties.Name -contains $Key)) {
            throw "Report row is missing required key '$Key'"
        }
        $k = [string]$row.$Key
        if ([string]::IsNullOrWhiteSpace($k)) {
            throw "Report row has an empty '$Key'"
        }
        if ($map.ContainsKey($k)) {
            throw "Report contains duplicate '$Key' value '$k'"
        }
        $map[$k] = $row
    }
    return $map
}

function Assert-SameKeySet {
    param(
        [hashtable]$Before,
        [hashtable]$After,
        [Parameter(Mandatory = $true)][string]$Context
    )

    $difference = Compare-Object `
        -ReferenceObject @($Before.Keys | Sort-Object) `
        -DifferenceObject @($After.Keys | Sort-Object)
    if ($difference) {
        throw "$Context baseline/current case sets differ"
    }
}

function Assert-Passed {
    param(
        [Parameter(Mandatory = $true)]$Row,
        [Parameter(Mandatory = $true)][string]$Context
    )

    if (!($Row.PSObject.Properties.Name -contains "passed") -or $Row.passed -isnot [bool] -or !$Row.passed) {
        throw "$Context is missing a passing verdict"
    }
}

function Add-Comparison {
    param(
        [System.Collections.ArrayList]$Out,
        [Parameter(Mandatory = $true)][string]$Stage,
        [Parameter(Mandatory = $true)][string]$Scenario,
        [Parameter(Mandatory = $true)][string]$Metric,
        [double]$Before,
        [double]$After,
        [Parameter(Mandatory = $true)][string]$Direction,
        [double]$TolerancePct = 0.0
    )
    if ($null -eq $Out) {
        return
    }

    if ($Before -eq 0) {
        $deltaPct = 0
    } else {
        $deltaPct = (($After - $Before) / $Before) * 100.0
    }

    $tolerance = [math]::Max($TolerancePct, 0.0) / 100.0
    $status = switch ($Direction) {
        "lower" {
            if ($Before -eq 0) {
                if ($After -eq 0) { "flat" } else { "regressed" }
            }
            elseif ($After -gt $Before * (1.0 + $tolerance)) {
                "regressed"
            }
            elseif ($After -lt $Before * (1.0 - $tolerance)) {
                "improved"
            }
            else {
                "flat"
            }
        }
        "higher" {
            if ($Before -eq 0) {
                if ($After -gt 0) { "improved" } else { "flat" }
            }
            elseif ($After -lt $Before * (1.0 - $tolerance)) {
                "regressed"
            }
            elseif ($After -gt $Before * (1.0 + $tolerance)) {
                "improved"
            }
            else {
                "flat"
            }
        }
        default { throw "Unknown metric direction: $Direction" }
    }

    [void]$Out.Add([pscustomobject]@{
            stage = $Stage
            scenario = $Scenario
            metric = $Metric
            before = [math]::Round($Before, 3)
            after = [math]::Round($After, 3)
            delta_pct = [math]::Round($deltaPct, 2)
            trend = $status
        })
}

$comparisons = [System.Collections.ArrayList]::new()

$beforeProject = Strict-ByKey -Rows (Read-JsonLines -Path (Join-Path $BeforeDir "project-lifecycle.jsonl")) -Key "case"
$afterProject = Strict-ByKey -Rows (Read-JsonLines -Path (Join-Path $AfterDir "project-lifecycle.jsonl")) -Key "case"
$expectedProjectCases = @(
    "project.create_new_project",
    "project.open_existing",
    "project.save_existing"
)
if ($beforeProject.Count -ne $expectedProjectCases.Count -or
    $afterProject.Count -ne $expectedProjectCases.Count) {
    throw "Project baseline/current must each contain exactly three cases"
}
Assert-SameKeySet -Before $beforeProject -After $afterProject -Context "Project report"
Assert-MondrianPerfCases -Cases @($beforeProject.Values) -Scenario "project"
Assert-MondrianPerfCases -Cases @($afterProject.Values) -Scenario "project"
Assert-MondrianIdenticalOwnerClosures `
    -Containers @($beforeProject.Values) `
    -Context "Project baseline cases"
Assert-MondrianIdenticalOwnerClosures `
    -Containers @($afterProject.Values) `
    -Context "Project current cases"
if (Compare-Object `
        -ReferenceObject ($expectedProjectCases | Sort-Object) `
        -DifferenceObject @($afterProject.Keys | Sort-Object)) {
    throw "Project report does not contain the expected lifecycle cases"
}

foreach ($key in $afterProject.Keys) {
    $before = $beforeProject[$key]
    $after = $afterProject[$key]
    Assert-Passed -Row $before -Context "Project baseline '$key'"
    Assert-Passed -Row $after -Context "Project current '$key'"
    Assert-MondrianCleanOwnerClosure -Container $before -Context "Project baseline '$key'" -ExpectedPreviewOwners 0 -GpuRequired $false
    Assert-MondrianCleanOwnerClosure -Container $after -Context "Project current '$key'" -ExpectedPreviewOwners 0 -GpuRequired $false
    if ([uint64]$before.iterations -ne [uint64]$after.iterations -or
        [uint64]$before.threshold_ms -ne [uint64]$after.threshold_ms) {
        throw "Project workload or threshold changed for '$key'"
    }
    Add-Comparison -Out $comparisons -Stage "project" -Scenario $key -Metric "avg_ms" -Before ([double]$before.avg_ms) -After ([double]$after.avg_ms) -Direction "lower" -TolerancePct $RegressionTolerancePct
    Add-Comparison -Out $comparisons -Stage "project" -Scenario $key -Metric "max_ms" -Before ([double]$before.max_ms) -After ([double]$after.max_ms) -Direction "lower" -TolerancePct $RegressionTolerancePct
}

$exportFiles = @(
    "export-1080p2997.jsonl",
    "export-4k60.jsonl",
    "export-4k60-passthrough.jsonl"
)
$beforeExportRows = @()
$afterExportRows = @()
foreach ($file in $exportFiles) {
    $beforePath = Join-Path $BeforeDir $file
    $afterPath = Join-Path $AfterDir $file
    $beforeExists = Test-Path -LiteralPath $beforePath -PathType Leaf
    $afterExists = Test-Path -LiteralPath $afterPath -PathType Leaf
    if ($beforeExists -ne $afterExists) {
        throw "Export baseline/current file sets differ at '$file'"
    }
    if ($beforeExists) {
        $beforeExportRows += @(Read-JsonLines -Path $beforePath)
        $afterExportRows += @(Read-JsonLines -Path $afterPath)
    }
}
$beforeExport = Strict-ByKey -Rows $beforeExportRows -Key "scenario"
$afterExport = Strict-ByKey -Rows $afterExportRows -Key "scenario"
Assert-SameKeySet -Before $beforeExport -After $afterExport -Context "Export report"
foreach ($key in $afterExport.Keys) {
    $before = $beforeExport[$key]
    $after = $afterExport[$key]
    Assert-Passed -Row $before -Context "Export baseline '$key'"
    Assert-Passed -Row $after -Context "Export current '$key'"
    $beforeWorkload = [pscustomobject]@{
        resolution = $before.resolution
        target_fps = $before.target_fps
        simulated_frames = $before.simulated_frames
        layers = $before.layers
        opacity_pattern = $before.opacity_pattern
        first_frame_threshold_ms = $before.first_frame_threshold_ms
        fps_min_threshold = $before.fps_min_threshold
        fps_max_threshold = $before.fps_max_threshold
    } | ConvertTo-Json -Compress
    $afterWorkload = [pscustomobject]@{
        resolution = $after.resolution
        target_fps = $after.target_fps
        simulated_frames = $after.simulated_frames
        layers = $after.layers
        opacity_pattern = $after.opacity_pattern
        first_frame_threshold_ms = $after.first_frame_threshold_ms
        fps_min_threshold = $after.fps_min_threshold
        fps_max_threshold = $after.fps_max_threshold
    } | ConvertTo-Json -Compress
    if ($beforeWorkload -ne $afterWorkload) {
        throw "Export workload or threshold changed for '$key'"
    }
    Add-Comparison -Out $comparisons -Stage "export" -Scenario $key -Metric "first_frame_ms" -Before ([double]$before.first_frame_ms) -After ([double]$after.first_frame_ms) -Direction "lower" -TolerancePct $RegressionTolerancePct
    Add-Comparison -Out $comparisons -Stage "export" -Scenario $key -Metric "frame_ms_avg" -Before ([double]$before.frame_ms_avg) -After ([double]$after.frame_ms_avg) -Direction "lower" -TolerancePct $RegressionTolerancePct
    Add-Comparison -Out $comparisons -Stage "export" -Scenario $key -Metric "achieved_fps" -Before ([double]$before.achieved_fps) -After ([double]$after.achieved_fps) -Direction "higher" -TolerancePct $RegressionTolerancePct
}

if ($comparisons.Count -eq 0) {
    throw "No comparable rows found; baseline/current evidence is incomplete"
}

$sorted = $comparisons | Sort-Object stage, scenario, metric
$sorted | Format-Table -AutoSize

$improved = @($sorted | Where-Object { $_.trend -eq "improved" }).Count
$regressed = @($sorted | Where-Object { $_.trend -eq "regressed" }).Count
$flat = @($sorted | Where-Object { $_.trend -eq "flat" }).Count

Write-Host ""
Write-Host "Summary:"
Write-Host "  improved : $improved"
Write-Host "  regressed: $regressed"
Write-Host "  flat     : $flat"
Write-Host "  tolerance: $RegressionTolerancePct%"

if ($FailOnRegression -and $regressed -gt 0) {
    exit 1
}
