param(
    [string]$OutputRoot = "tests/fixtures/large",
    [switch]$Force,
    [switch]$ValidatePrerequisitesOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$FixtureId = "generated-golden-rec709-h264-vfr-v1"
$FileName = "golden-rec709-h264-vfr-v1.mp4"
$RecipePath = "scripts/validation/generate-golden-vfr-retime-video.ps1"

function Resolve-RepositoryPath([string]$Path) {
    $repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Path))
}

function Require-Command([string]$Name) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        throw "Required command '$Name' is unavailable."
    }
    return $command.Source
}

function Invoke-Checked([string]$Command, [string[]]$Arguments) {
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $Command $($Arguments -join ' ')"
    }
}

function Read-JsonProbe([string]$Ffprobe, [string]$ArtifactPath) {
    $json = & $Ffprobe @(
        "-v", "error",
        "-select_streams", "v:0",
        "-show_frames",
        "-show_entries", "format=duration:stream=index,codec_name,profile,pix_fmt,width,height,r_frame_rate,avg_frame_rate,time_base,color_range,color_space,color_transfer,color_primaries,duration,nb_frames:frame=best_effort_timestamp,pkt_duration",
        "-of", "json",
        $ArtifactPath
    )
    if ($LASTEXITCODE -ne 0) {
        throw "ffprobe failed for $ArtifactPath"
    }
    return ($json -join [Environment]::NewLine) | ConvertFrom-Json
}

function Assert-Probe([object]$Probe) {
    $video = @($Probe.streams | Where-Object codec_name -eq "h264")
    if ($video.Count -ne 1 -or @($Probe.streams).Count -ne 1) {
        throw "Generated Golden VFR fixture must contain exactly one H.264 video stream."
    }
    $stream = $video[0]
    if (
        [string]$stream.profile -ne "High" -or
        [string]$stream.pix_fmt -ne "yuv420p" -or
        [int]$stream.width -ne 1920 -or
        [int]$stream.height -ne 1080 -or
        [string]$stream.color_range -ne "tv" -or
        [string]$stream.color_space -ne "bt709" -or
        [string]$stream.color_transfer -ne "bt709" -or
        [string]$stream.color_primaries -ne "bt709"
    ) {
        throw "Generated Golden VFR fixture does not satisfy the H.264 High Rec.709 contract."
    }
    $frames = @($Probe.frames)
    if ($frames.Count -ne 200 -or [int]$stream.nb_frames -ne 200) {
        throw "Generated Golden VFR fixture must contain exactly 200 decoded presentation frames."
    }
    $timestamps = @($frames | ForEach-Object { [int64]$_.best_effort_timestamp })
    $deltas = for ($index = 1; $index -lt $timestamps.Count; $index++) {
        $timestamps[$index] - $timestamps[$index - 1]
    }
    $durations = @($frames | ForEach-Object { [int64]$_.pkt_duration })
    if (@($deltas | Where-Object { $_ -le 0 }).Count -ne 0) {
        throw "Generated Golden VFR fixture PTS must be strictly monotonic."
    }
    $cadences = @($deltas | Sort-Object -Unique)
    if ($cadences.Count -ne 2 -or $cadences[1] -ne 3 * $cadences[0]) {
        throw "Generated Golden VFR fixture must alternate two presentation intervals with a 1:3 duration ratio."
    }
    foreach ($cadence in $cadences) {
        if (@($deltas | Where-Object { $_ -eq $cadence }).Count -lt 90) {
            throw "Generated Golden VFR fixture does not contain enough observations of cadence $cadence."
        }
    }
    for ($index = 0; $index -lt $deltas.Count; $index++) {
        if ($durations[$index] -ne $deltas[$index]) {
            throw "Generated Golden VFR fixture has a presentation gap at frame ${index}: duration $($durations[$index]), successor delta $($deltas[$index])."
        }
    }
    if (@($durations | Where-Object { $_ -le 0 }).Count -ne 0) {
        throw "Generated Golden VFR fixture contains a non-positive packet duration."
    }
    if ([string]$stream.r_frame_rate -eq [string]$stream.avg_frame_rate) {
        throw "Generated Golden VFR fixture does not prove variable cadence in stream metadata."
    }
    if ([double]$Probe.format.duration -lt 7.9) {
        throw "Generated Golden VFR fixture is shorter than 7.9 seconds."
    }
}

function Write-Attestation(
    [string]$ArtifactPath,
    [string]$FfmpegVersion,
    [string]$FfprobeVersion,
    [object]$Probe
) {
    $scriptPath = [IO.Path]::GetFullPath($PSCommandPath)
    $artifact = Get-Item -LiteralPath $ArtifactPath
    $attestation = [ordered]@{
        schema_version = 1
        fixture_id = $FixtureId
        generated_at_utc = [DateTime]::UtcNow.ToString("o")
        recipe = [ordered]@{
            path = $RecipePath
            sha256 = (Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        tools = [ordered]@{
            ffmpeg = $FfmpegVersion
            ffprobe = $FfprobeVersion
        }
        artifact = [ordered]@{
            file_name = $artifact.Name
            size_bytes = [int64]$artifact.Length
            sha256 = (Get-FileHash -LiteralPath $artifact.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        probe = $Probe
    }
    $attestationPath = "$ArtifactPath.attestation.json"
    $attestation | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $attestationPath -Encoding utf8
    Write-Host "Generated $FixtureId"
    Write-Host "  artifact: $ArtifactPath"
    Write-Host "  attestation: $attestationPath"
}

function Assert-ExistingAttestation([string]$ArtifactPath, [string]$RecipeHash) {
    $attestationPath = "$ArtifactPath.attestation.json"
    if (-not (Test-Path -LiteralPath $attestationPath -PathType Leaf)) {
        throw "Existing Golden VFR fixture has no attestation. Re-run with -Force."
    }
    $attestation = Get-Content -Raw -LiteralPath $attestationPath | ConvertFrom-Json
    $artifact = Get-Item -LiteralPath $ArtifactPath
    $artifactHash = (Get-FileHash -LiteralPath $ArtifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if (
        [int]$attestation.schema_version -ne 1 -or
        [string]$attestation.fixture_id -ne $FixtureId -or
        [string]$attestation.recipe.path -ne $RecipePath -or
        [string]$attestation.recipe.sha256 -ne $RecipeHash -or
        [string]$attestation.artifact.file_name -ne $artifact.Name -or
        [int64]$attestation.artifact.size_bytes -ne [int64]$artifact.Length -or
        [string]$attestation.artifact.sha256 -ne $artifactHash
    ) {
        throw "Existing Golden VFR fixture is not attested for the current recipe and artifact. Re-run with -Force."
    }
}

$ffmpeg = Require-Command "ffmpeg"
$ffprobe = Require-Command "ffprobe"
$ffmpegVersion = ((& $ffmpeg -version 2>&1 | Select-Object -First 1) -join "").Trim()
$ffprobeVersion = ((& $ffprobe -version 2>&1 | Select-Object -First 1) -join "").Trim()
$encoderList = ((& $ffmpeg -hide_banner -encoders 2>&1) -join [Environment]::NewLine)
if ($encoderList -notmatch "\blibx264\b") {
    throw "The Golden VFR recipe requires the FFmpeg libx264 encoder."
}
$filterList = ((& $ffmpeg -hide_banner -filters 2>&1) -join [Environment]::NewLine)
if (
    $filterList -notmatch "\btestsrc2\b" -or
    $filterList -notmatch "\bsettb\b" -or
    $filterList -notmatch "\bsetpts\b"
) {
    throw "The Golden VFR recipe requires FFmpeg testsrc2, settb, and setpts support."
}
if ($ValidatePrerequisitesOnly) {
    Write-Host "Golden VFR retime video generator prerequisites are available."
    return
}

$outputDirectory = Resolve-RepositoryPath $OutputRoot
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
$artifactPath = Join-Path $outputDirectory $FileName
$shouldGenerate = $Force -or -not (Test-Path -LiteralPath $artifactPath -PathType Leaf)
if ($shouldGenerate) {
    $partialPath = "$artifactPath.partial.mp4"
    Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
    try {
        Invoke-Checked $ffmpeg @(
            "-hide_banner", "-nostdin", "-loglevel", "warning", "-y",
            "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=50:duration=4",
            "-an",
            "-vf", "settb=1/12800,setpts='floor(N/2)*1024+mod(N\,2)*256',format=yuv420p",
            "-fps_mode", "vfr",
            "-c:v", "libx264",
            "-bf", "0",
            "-preset", "veryfast",
            "-crf", "18",
            "-profile:v", "high",
            "-level:v", "4.2",
            "-g", "50",
            "-keyint_min", "50",
            "-sc_threshold", "0",
            "-video_track_timescale", "12800",
            "-color_range", "tv",
            "-colorspace", "bt709",
            "-color_trc", "bt709",
            "-color_primaries", "bt709",
            "-movflags", "+faststart",
            $partialPath
        )
        Move-Item -LiteralPath $partialPath -Destination $artifactPath -Force
    } finally {
        Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
    }
}

$probe = Read-JsonProbe $ffprobe $artifactPath
Assert-Probe $probe
if ($shouldGenerate) {
    Write-Attestation $artifactPath $ffmpegVersion $ffprobeVersion $probe
} else {
    $recipeHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-ExistingAttestation $artifactPath $recipeHash
    Write-Host "Reused attested $FixtureId"
    Write-Host "  artifact: $artifactPath"
    Write-Host "  attestation: $artifactPath.attestation.json"
}
