param(
    [string]$OutputRoot = "tests/fixtures/large",
    [switch]$Force,
    [switch]$ValidatePrerequisitesOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$FixtureId = "generated-golden-pcm-s16-48k-stereo-305s-v1"
$FileName = "golden-pcm-s16-48k-stereo-305s-v1.mov"

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
        "-show_entries", "format=duration:stream=index,codec_name,sample_fmt,sample_rate,channels,channel_layout,duration",
        "-of", "json",
        $ArtifactPath
    )
    if ($LASTEXITCODE -ne 0) {
        throw "ffprobe failed for $ArtifactPath"
    }
    return ($json -join [Environment]::NewLine) | ConvertFrom-Json
}

function Assert-Probe([object]$Probe) {
    $audio = @($Probe.streams | Where-Object codec_name -eq "pcm_s16le")
    if ($audio.Count -ne 1) {
        throw "Generated Golden PCM fixture must contain exactly one PCM S16LE stream."
    }
    $stream = $audio[0]
    if (
        $stream.sample_fmt -ne "s16" -or
        [int]$stream.sample_rate -ne 48000 -or
        [int]$stream.channels -ne 2 -or
        [string]$stream.channel_layout -ne "stereo"
    ) {
        throw "Generated Golden PCM fixture is not signed 16-bit, 48 kHz stereo."
    }
    if ([double]$stream.duration -lt 304.9) {
        throw "Generated Golden PCM fixture is shorter than 304.9 seconds."
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
            path = "scripts/validation/generate-golden-project-media.ps1"
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
        throw "Existing Golden PCM fixture has no attestation. Re-run with -Force."
    }
    $attestation = Get-Content -Raw -LiteralPath $attestationPath | ConvertFrom-Json
    $artifact = Get-Item -LiteralPath $ArtifactPath
    $artifactHash = (Get-FileHash -LiteralPath $ArtifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if (
        [int]$attestation.schema_version -ne 1 -or
        [string]$attestation.fixture_id -ne $FixtureId -or
        [string]$attestation.recipe.path -ne "scripts/validation/generate-golden-project-media.ps1" -or
        [string]$attestation.recipe.sha256 -ne $RecipeHash -or
        [string]$attestation.artifact.file_name -ne $artifact.Name -or
        [int64]$attestation.artifact.size_bytes -ne [int64]$artifact.Length -or
        [string]$attestation.artifact.sha256 -ne $artifactHash
    ) {
        throw "Existing Golden PCM fixture is not attested for the current recipe and artifact. Re-run with -Force."
    }
}

$ffmpeg = Require-Command "ffmpeg"
$ffprobe = Require-Command "ffprobe"
$ffmpegVersion = ((& $ffmpeg -version 2>&1 | Select-Object -First 1) -join "").Trim()
$ffprobeVersion = ((& $ffprobe -version 2>&1 | Select-Object -First 1) -join "").Trim()
$filterList = ((& $ffmpeg -hide_banner -filters 2>&1) -join [Environment]::NewLine)
if ($filterList -notmatch "\baevalsrc\b") {
    throw "The Golden PCM recipe requires FFmpeg aevalsrc support."
}
if ($ValidatePrerequisitesOnly) {
    Write-Host "Golden Project media generator prerequisites are available."
    return
}

$outputDirectory = Resolve-RepositoryPath $OutputRoot
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
$artifactPath = Join-Path $outputDirectory $FileName
$shouldGenerate = $Force -or -not (Test-Path -LiteralPath $artifactPath -PathType Leaf)
if ($shouldGenerate) {
    $partialPath = "$artifactPath.partial.mov"
    Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
    try {
        # The unequal, analytically defined channels make pan, gain, fades, and
        # accidental channel swaps observable without relying on copyrighted media.
        $signal = "aevalsrc=exprs=0.10*sin(2*PI*997*t)+0.025*sin(2*PI*73*t)|0.08*sin(2*PI*1597*t)+0.020*sin(2*PI*109*t):s=48000:d=305"
        Invoke-Checked $ffmpeg @(
            "-hide_banner", "-nostdin", "-loglevel", "warning", "-y",
            "-f", "lavfi", "-i", $signal,
            "-vn", "-c:a", "pcm_s16le", "-ar", "48000", "-channel_layout", "stereo",
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
