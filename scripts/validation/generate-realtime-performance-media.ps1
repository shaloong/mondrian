param(
    [string]$OutputRoot = "tests/fixtures/large",
    [switch]$Force,
    [switch]$ValidatePrerequisitesOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$FixtureId = "generated-performance-4k60-hevc-main10-rec709-v1"
$FileName = "performance-4k60-hevc-main10-rec709-12s-v1.mp4"
$RecipePath = "scripts/validation/generate-realtime-performance-media.ps1"

function Resolve-RepositoryPath([string]$Path) {
    $repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $repositoryRoot $Path))
}

function Require-Command([string]$Name) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($null -eq $command) { throw "Required command '$Name' is unavailable." }
    return $command.Source
}

function Invoke-Checked([string]$Command, [string[]]$Arguments) {
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $Command $($Arguments -join ' ')"
    }
}

function Read-Probe([string]$Ffprobe, [string]$ArtifactPath) {
    $json = & $Ffprobe @(
        "-v", "error", "-show_entries",
        "format=duration:stream=index,codec_name,profile,width,height,pix_fmt,avg_frame_rate,r_frame_rate,duration,nb_frames,color_range,color_space,color_transfer,color_primaries",
        "-of", "json", $ArtifactPath
    )
    if ($LASTEXITCODE -ne 0) { throw "ffprobe failed for $ArtifactPath" }
    return ($json -join [Environment]::NewLine) | ConvertFrom-Json
}

function Assert-Probe([object]$Probe) {
    $video = @($Probe.streams | Where-Object codec_name -eq "hevc")
    if ($video.Count -ne 1) { throw "Realtime fixture must contain exactly one HEVC stream." }
    $stream = $video[0]
    if ($stream.profile -notmatch "Main 10" -or $stream.pix_fmt -notmatch "10") {
        throw "Realtime fixture is not decoder-proven HEVC Main10."
    }
    if ([int]$stream.width -ne 3840 -or [int]$stream.height -ne 2160) {
        throw "Realtime fixture is not 3840x2160."
    }
    if ($stream.avg_frame_rate -ne "60/1" -or [double]$stream.duration -lt 11.9) {
        throw "Realtime fixture must be constant 60/1 fps for at least 11.9 seconds."
    }
    if ($stream.color_primaries -ne "bt709" -or $stream.color_transfer -ne "bt709" -or $stream.color_space -ne "bt709" -or $stream.color_range -ne "tv") {
        throw "Realtime fixture does not retain limited-range Rec.709 metadata."
    }
}

function Assert-ExistingAttestation([string]$ArtifactPath, [string]$RecipeHash) {
    $attestationPath = "$ArtifactPath.attestation.json"
    if (-not (Test-Path -LiteralPath $attestationPath -PathType Leaf)) {
        throw "Existing realtime fixture has no attestation. Re-run with -Force."
    }
    $attestation = Get-Content -Raw -LiteralPath $attestationPath | ConvertFrom-Json
    $artifact = Get-Item -LiteralPath $ArtifactPath
    $artifactHash = (Get-FileHash -LiteralPath $ArtifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if (
        [int]$attestation.schema_version -ne 1 -or
        [string]$attestation.fixture_id -ne $FixtureId -or
        [string]$attestation.recipe.path -ne $RecipePath -or
        [string]$attestation.recipe.sha256 -ne $RecipeHash -or
        [int64]$attestation.artifact.size_bytes -ne [int64]$artifact.Length -or
        [string]$attestation.artifact.sha256 -ne $artifactHash
    ) { throw "Existing realtime fixture is not attested for the current recipe and artifact." }
}

$ffmpeg = Require-Command "ffmpeg"
$ffprobe = Require-Command "ffprobe"
$encoderList = ((& $ffmpeg -hide_banner -encoders 2>&1) -join [Environment]::NewLine)
$filterList = ((& $ffmpeg -hide_banner -filters 2>&1) -join [Environment]::NewLine)
if ($encoderList -notmatch "\blibx265\b" -or $filterList -notmatch "\btestsrc2\b") {
    throw "The realtime fixture recipe requires FFmpeg libx265 and testsrc2 support."
}
if ($ValidatePrerequisitesOnly) {
    Write-Host "Realtime performance fixture prerequisites are available."
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
            "-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=60:duration=12",
            "-an", "-vf", "format=yuv420p10le", "-c:v", "libx265", "-preset", "ultrafast",
            "-pix_fmt", "yuv420p10le",
            "-x265-params", "keyint=240:min-keyint=240:scenecut=0:open-gop=0:repeat-headers=1",
            "-tag:v", "hvc1", "-color_range", "tv", "-colorspace", "bt709",
            "-color_primaries", "bt709", "-color_trc", "bt709", "-movflags", "+faststart",
            $partialPath
        )
        Move-Item -LiteralPath $partialPath -Destination $artifactPath -Force
    } finally {
        Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
    }
}

$probe = Read-Probe $ffprobe $artifactPath
Assert-Probe $probe
$recipeHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($shouldGenerate) {
    $artifact = Get-Item -LiteralPath $artifactPath
    $attestation = [ordered]@{
        schema_version = 1
        fixture_id = $FixtureId
        generated_at_utc = [DateTime]::UtcNow.ToString("o")
        recipe = [ordered]@{ path = $RecipePath; sha256 = $recipeHash }
        tools = [ordered]@{
            ffmpeg = ((& $ffmpeg -version 2>&1 | Select-Object -First 1) -join "").Trim()
            ffprobe = ((& $ffprobe -version 2>&1 | Select-Object -First 1) -join "").Trim()
        }
        artifact = [ordered]@{
            file_name = $artifact.Name
            size_bytes = [int64]$artifact.Length
            sha256 = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        probe = $probe
    }
    $attestation | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath "$artifactPath.attestation.json" -Encoding utf8
    Write-Host "Generated $FixtureId at $artifactPath"
} else {
    Assert-ExistingAttestation $artifactPath $recipeHash
    Write-Host "Reused attested $FixtureId"
}
