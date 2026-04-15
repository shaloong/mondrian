param(
    [Parameter(Mandatory = $true)][string]$BeforeDir,
    [Parameter(Mandatory = $true)][string]$AfterDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

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

function Latest-ByKey {
    param(
        [array]$Rows = @(),
        [Parameter(Mandatory = $true)][string]$Key
    )
    $map = @{}
    if ($null -eq $Rows) {
        return $map
    }
    foreach ($row in $Rows) {
        $k = [string]$row.$Key
        $map[$k] = $row
    }
    return $map
}

function Add-Comparison {
    param(
        [System.Collections.ArrayList]$Out,
        [Parameter(Mandatory = $true)][string]$Stage,
        [Parameter(Mandatory = $true)][string]$Scenario,
        [Parameter(Mandatory = $true)][string]$Metric,
        [double]$Before,
        [double]$After,
        [Parameter(Mandatory = $true)][string]$Direction
    )
    if ($null -eq $Out) {
        return
    }

    if ($Before -eq 0) {
        $deltaPct = 0
    } else {
        $deltaPct = (($After - $Before) / $Before) * 100.0
    }

    $better = switch ($Direction) {
        "lower" { $After -lt $Before }
        "higher" { $After -gt $Before }
        default { $false }
    }
    $status = if ($better) { "improved" } elseif ($After -eq $Before) { "flat" } else { "regressed" }

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

$beforeProject = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $BeforeDir "project-lifecycle.jsonl")) -Key "case"
$afterProject = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $AfterDir "project-lifecycle.jsonl")) -Key "case"

foreach ($key in $afterProject.Keys) {
    if (!$beforeProject.ContainsKey($key)) {
        continue
    }
    Add-Comparison -Out $comparisons -Stage "project" -Scenario $key -Metric "avg_ms" -Before ([double]$beforeProject[$key].avg_ms) -After ([double]$afterProject[$key].avg_ms) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "project" -Scenario $key -Metric "max_ms" -Before ([double]$beforeProject[$key].max_ms) -After ([double]$afterProject[$key].max_ms) -Direction "lower"
}

$beforePreview = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $BeforeDir "preview.jsonl")) -Key "scenario"
$afterPreview = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $AfterDir "preview.jsonl")) -Key "scenario"
foreach ($key in $afterPreview.Keys) {
    if (!$beforePreview.ContainsKey($key)) {
        continue
    }
    Add-Comparison -Out $comparisons -Stage "preview" -Scenario $key -Metric "first_frame_ms" -Before ([double]$beforePreview[$key].first_frame_ms) -After ([double]$afterPreview[$key].first_frame_ms) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "preview" -Scenario $key -Metric "frame_ms_avg" -Before ([double]$beforePreview[$key].frame_ms_avg) -After ([double]$afterPreview[$key].frame_ms_avg) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "preview" -Scenario $key -Metric "achieved_fps" -Before ([double]$beforePreview[$key].achieved_fps) -After ([double]$afterPreview[$key].achieved_fps) -Direction "higher"
}

$beforeExport = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $BeforeDir "export.jsonl")) -Key "scenario"
$afterExport = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $AfterDir "export.jsonl")) -Key "scenario"
foreach ($key in $afterExport.Keys) {
    if (!$beforeExport.ContainsKey($key)) {
        continue
    }
    Add-Comparison -Out $comparisons -Stage "export" -Scenario $key -Metric "first_frame_ms" -Before ([double]$beforeExport[$key].first_frame_ms) -After ([double]$afterExport[$key].first_frame_ms) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "export" -Scenario $key -Metric "frame_ms_avg" -Before ([double]$beforeExport[$key].frame_ms_avg) -After ([double]$afterExport[$key].frame_ms_avg) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "export" -Scenario $key -Metric "achieved_fps" -Before ([double]$beforeExport[$key].achieved_fps) -After ([double]$afterExport[$key].achieved_fps) -Direction "higher"
}

$beforeAudio = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $BeforeDir "audio.jsonl")) -Key "scenario"
$afterAudio = Latest-ByKey -Rows (Read-JsonLines -Path (Join-Path $AfterDir "audio.jsonl")) -Key "scenario"
foreach ($key in $afterAudio.Keys) {
    if (!$beforeAudio.ContainsKey($key)) {
        continue
    }
    Add-Comparison -Out $comparisons -Stage "audio" -Scenario $key -Metric "first_chunk_ms" -Before ([double]$beforeAudio[$key].first_chunk_ms) -After ([double]$afterAudio[$key].first_chunk_ms) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "audio" -Scenario $key -Metric "chunk_ms_avg" -Before ([double]$beforeAudio[$key].chunk_ms_avg) -After ([double]$afterAudio[$key].chunk_ms_avg) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "audio" -Scenario $key -Metric "chunk_ms_p95" -Before ([double]$beforeAudio[$key].chunk_ms_p95) -After ([double]$afterAudio[$key].chunk_ms_p95) -Direction "lower"
    Add-Comparison -Out $comparisons -Stage "audio" -Scenario $key -Metric "realtime_factor" -Before ([double]$beforeAudio[$key].realtime_factor) -After ([double]$afterAudio[$key].realtime_factor) -Direction "higher"
}

if ($comparisons.Count -eq 0) {
    Write-Host "No comparable rows found. Check baseline/current directories and jsonl files."
    exit 0
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
