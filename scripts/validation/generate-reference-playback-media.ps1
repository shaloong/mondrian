param(
    [ValidateSet("All", "Video", "Audio")][string]$Profile = "All",
    [string]$OutputRoot = "tests/fixtures/large",
    [switch]$Force,
    [switch]$ValidatePrerequisitesOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$VideoFixtureId = "generated-playback-4k25-hevc-main10-rec709-v1"
$AudioFixtureId = "generated-playback-aac-48k-stereo-v1"
$VideoFileName = "playback-4k25-hevc-main10-rec709-1812s-v1.mp4"
$AudioFileName = "playback-aac-48k-stereo-1835s-v1.m4a"

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

function Write-Attestation(
    [string]$FixtureId,
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
            path = "scripts/validation/generate-reference-playback-media.ps1"
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

function Read-JsonProbe([string]$Ffprobe, [string]$ArtifactPath) {
    $json = & $Ffprobe @(
        "-v", "error",
        "-show_entries", "format=duration:stream=index,codec_name,profile,width,height,pix_fmt,avg_frame_rate,r_frame_rate,duration,nb_frames,sample_rate,channels,channel_layout,color_range,color_space,color_transfer,color_primaries",
        "-of", "json",
        $ArtifactPath
    )
    if ($LASTEXITCODE -ne 0) {
        throw "ffprobe failed for $ArtifactPath"
    }
    return ($json -join [Environment]::NewLine) | ConvertFrom-Json
}

function Assert-VideoProbe([object]$Probe) {
    $video = @($Probe.streams | Where-Object codec_name -eq "hevc")
    if ($video.Count -ne 1) { throw "Generated video must contain exactly one HEVC stream." }
    $stream = $video[0]
    if ($stream.profile -notmatch "Main 10") { throw "Generated video profile is not HEVC Main 10: $($stream.profile)" }
    if ([int]$stream.width -ne 3840 -or [int]$stream.height -ne 2160) { throw "Generated video is not 3840x2160." }
    if ($stream.pix_fmt -notmatch "10") { throw "Generated video pixel format is not decoder-proven 10-bit: $($stream.pix_fmt)" }
    if ($stream.avg_frame_rate -ne "25/1") { throw "Generated video average frame rate is not 25/1: $($stream.avg_frame_rate)" }
    if ([double]$stream.duration -lt 1811.9) { throw "Generated video stream is shorter than 1811.9 seconds." }
    if ($stream.color_primaries -ne "bt709" -or $stream.color_transfer -ne "bt709" -or $stream.color_space -ne "bt709" -or $stream.color_range -ne "tv") {
        throw "Generated video does not retain the declared limited-range Rec.709 code contract."
    }
}

function Assert-AudioProbe([object]$Probe) {
    $audio = @($Probe.streams | Where-Object codec_name -eq "aac")
    if ($audio.Count -ne 1) { throw "Generated audio must contain exactly one AAC stream." }
    $stream = $audio[0]
    if ([int]$stream.sample_rate -ne 48000 -or [int]$stream.channels -ne 2) {
        throw "Generated audio is not 48 kHz stereo."
    }
    if ([double]$stream.duration -lt 1834.9) { throw "Generated audio stream is shorter than 1834.9 seconds." }
}

$ffmpeg = Require-Command "ffmpeg"
$ffprobe = Require-Command "ffprobe"
$ffmpegVersion = ((& $ffmpeg -version 2>&1 | Select-Object -First 1) -join "").Trim()
$ffprobeVersion = ((& $ffprobe -version 2>&1 | Select-Object -First 1) -join "").Trim()
$encoderList = ((& $ffmpeg -hide_banner -encoders 2>&1) -join [Environment]::NewLine)
$filterList = ((& $ffmpeg -hide_banner -filters 2>&1) -join [Environment]::NewLine)
if ($Profile -in @("All", "Video") -and ($encoderList -notmatch "\blibx265\b" -or $filterList -notmatch "\btestsrc2\b")) {
    throw "The Video recipe requires FFmpeg libx265 and testsrc2 support."
}
if ($Profile -in @("All", "Audio") -and ($encoderList -notmatch "\baac\b" -or $filterList -notmatch "\baevalsrc\b")) {
    throw "The Audio recipe requires FFmpeg AAC and aevalsrc support."
}
if ($ValidatePrerequisitesOnly) {
    Write-Host "Reference playback generator prerequisites are available for profile $Profile."
    return
}
$outputDirectory = Resolve-RepositoryPath $OutputRoot
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null

if ($Profile -in @("All", "Video")) {
    $videoPath = Join-Path $outputDirectory $VideoFileName
    if ($Force -or -not (Test-Path -LiteralPath $videoPath -PathType Leaf)) {
        $seedPath = Join-Path $outputDirectory ".playback-4k25-hevc-main10-seed.partial.mp4"
        $partialPath = "$videoPath.partial.mp4"
        Remove-Item -LiteralPath $seedPath, $partialPath -Force -ErrorAction SilentlyContinue
        try {
            # testsrc2 is an encoded-code stress stimulus, not a color reference.
            # Its values are deliberately tagged as limited-range Rec.709 and this
            # fixture is forbidden from satisfying any color-correctness purpose.
            Invoke-Checked $ffmpeg @(
                "-hide_banner", "-nostdin", "-loglevel", "warning", "-y",
                "-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=25:duration=12",
                "-an", "-vf", "format=yuv420p10le",
                "-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le",
                "-x265-params", "keyint=250:min-keyint=250:scenecut=0:open-gop=0:repeat-headers=1",
                "-tag:v", "hvc1", "-color_range", "tv", "-colorspace", "bt709",
                "-color_primaries", "bt709", "-color_trc", "bt709",
                $seedPath
            )
            Invoke-Checked $ffmpeg @(
                "-hide_banner", "-nostdin", "-loglevel", "warning", "-y",
                "-stream_loop", "150", "-i", $seedPath, "-map", "0:v:0",
                "-t", "1812", "-c", "copy", "-movflags", "+faststart", $partialPath
            )
            Move-Item -LiteralPath $partialPath -Destination $videoPath -Force
        } finally {
            Remove-Item -LiteralPath $seedPath, $partialPath -Force -ErrorAction SilentlyContinue
        }
    }
    $videoProbe = Read-JsonProbe $ffprobe $videoPath
    Assert-VideoProbe $videoProbe
    Write-Attestation $VideoFixtureId $videoPath $ffmpegVersion $ffprobeVersion $videoProbe
}

if ($Profile -in @("All", "Audio")) {
    $audioPath = Join-Path $outputDirectory $AudioFileName
    if ($Force -or -not (Test-Path -LiteralPath $audioPath -PathType Leaf)) {
        $partialPath = "$audioPath.partial.m4a"
        Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
        try {
            $signal = "aevalsrc=exprs=0.08*sin(2*PI*997*t)+0.025*sin(2*PI*73*t)|0.08*sin(2*PI*1597*t)+0.025*sin(2*PI*109*t):s=48000:d=1835"
            Invoke-Checked $ffmpeg @(
                "-hide_banner", "-nostdin", "-loglevel", "warning", "-y",
                "-f", "lavfi", "-i", $signal, "-vn", "-c:a", "aac", "-b:a", "192k",
                "-ar", "48000", "-ac", "2", "-movflags", "+faststart", $partialPath
            )
            Move-Item -LiteralPath $partialPath -Destination $audioPath -Force
        } finally {
            Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
        }
    }
    $audioProbe = Read-JsonProbe $ffprobe $audioPath
    Assert-AudioProbe $audioProbe
    Write-Attestation $AudioFixtureId $audioPath $ffmpegVersion $ffprobeVersion $audioProbe
}
