param(
    [string]$OutputRoot = "tests/fixtures/large",
    [ValidateSet("All", "HlgMain10", "SrgbAlpha")]
    [string]$Profile = "All",
    [switch]$Force,
    [switch]$ValidatePrerequisitesOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RecipePath = "scripts/validation/generate-golden-color-reference-media.ps1"
$HlgFixtureId = "generated-golden-hlg-main10-25fps-v1"
$HlgFileName = "golden-hlg-main10-25fps-v1.mp4"
$SrgbFixtureId = "generated-golden-srgb-alpha-still-v1"
$SrgbFileName = "golden-srgb-alpha-still-v1.png"

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
        "-show_entries", "format=duration:stream=index,codec_name,profile,pix_fmt,width,height,avg_frame_rate,color_range,color_space,color_transfer,color_primaries,duration",
        "-of", "json",
        $ArtifactPath
    )
    if ($LASTEXITCODE -ne 0) {
        throw "ffprobe failed for $ArtifactPath"
    }
    return ($json -join [Environment]::NewLine) | ConvertFrom-Json
}

function Assert-HlgProbe([object]$Probe) {
    $streams = @($Probe.streams)
    if ($streams.Count -ne 1 -or [string]$streams[0].codec_name -ne "hevc") {
        throw "Generated HLG fixture must contain exactly one HEVC video stream."
    }
    $stream = $streams[0]
    if (
        [string]$stream.profile -ne "Main 10" -or
        [string]$stream.pix_fmt -ne "yuv420p10le" -or
        [int]$stream.width -ne 1920 -or
        [int]$stream.height -ne 1080 -or
        [string]$stream.avg_frame_rate -ne "25/1" -or
        [string]$stream.color_range -ne "tv" -or
        [string]$stream.color_space -ne "bt2020nc" -or
        [string]$stream.color_transfer -ne "arib-std-b67" -or
        [string]$stream.color_primaries -ne "bt2020"
    ) {
        throw "Generated HLG fixture does not satisfy the HEVC Main10 BT.2100 HLG contract."
    }
    if ([double]$Probe.format.duration -lt 0.99) {
        throw "Generated HLG fixture is shorter than 0.99 seconds."
    }
}

function Assert-SrgbProbe([object]$Probe) {
    $streams = @($Probe.streams)
    if ($streams.Count -ne 1 -or [string]$streams[0].codec_name -ne "png") {
        throw "Generated sRGB Alpha fixture must contain exactly one PNG video stream."
    }
    $stream = $streams[0]
    if (
        [string]$stream.pix_fmt -ne "rgba" -or
        [int]$stream.width -ne 1920 -or
        [int]$stream.height -ne 1080 -or
        [string]$stream.color_range -ne "pc" -or
        [string]$stream.color_space -ne "gbr" -or
        [string]$stream.color_transfer -ne "iec61966-2-1" -or
        [string]$stream.color_primaries -ne "bt709"
    ) {
        throw "Generated sRGB Alpha fixture does not satisfy the full-range RGBA sRGB contract."
    }
}

function Write-Attestation(
    [string]$FixtureId,
    [string]$ProfileName,
    [string]$ArtifactPath,
    [string]$FfmpegVersion,
    [string]$FfprobeVersion,
    [object]$Probe
) {
    $artifact = Get-Item -LiteralPath $ArtifactPath
    $attestation = [ordered]@{
        schema_version = 1
        fixture_id = $FixtureId
        generated_at_utc = [DateTime]::UtcNow.ToString("o")
        recipe = [ordered]@{
            path = $RecipePath
            sha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
            profile = $ProfileName
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

function Assert-ExistingAttestation(
    [string]$FixtureId,
    [string]$ProfileName,
    [string]$ArtifactPath,
    [string]$RecipeHash
) {
    $attestationPath = "$ArtifactPath.attestation.json"
    if (-not (Test-Path -LiteralPath $attestationPath -PathType Leaf)) {
        throw "Existing fixture '$FixtureId' has no attestation. Re-run with -Force."
    }
    $attestation = Get-Content -Raw -LiteralPath $attestationPath | ConvertFrom-Json
    $artifact = Get-Item -LiteralPath $ArtifactPath
    $artifactHash = (Get-FileHash -LiteralPath $ArtifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if (
        [int]$attestation.schema_version -ne 1 -or
        [string]$attestation.fixture_id -ne $FixtureId -or
        [string]$attestation.recipe.path -ne $RecipePath -or
        [string]$attestation.recipe.sha256 -ne $RecipeHash -or
        [string]$attestation.recipe.profile -ne $ProfileName -or
        [string]$attestation.artifact.file_name -ne $artifact.Name -or
        [int64]$attestation.artifact.size_bytes -ne [int64]$artifact.Length -or
        [string]$attestation.artifact.sha256 -ne $artifactHash
    ) {
        throw "Existing fixture '$FixtureId' is not attested for the current recipe and artifact. Re-run with -Force."
    }
}

function Add-PngSrgbChunk([string]$Path) {
    if ($null -eq ("MondrianPngSrgbChunk" -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;

public static class MondrianPngSrgbChunk
{
    private static uint Crc32(byte[] bytes)
    {
        uint crc = 0xffffffffu;
        foreach (byte value in bytes)
        {
            crc ^= value;
            for (int bit = 0; bit < 8; bit++)
                crc = (crc & 1u) != 0 ? 0xedb88320u ^ (crc >> 1) : crc >> 1;
        }
        return crc ^ 0xffffffffu;
    }

    private static void WriteBigEndian(Stream stream, uint value)
    {
        stream.WriteByte((byte)(value >> 24));
        stream.WriteByte((byte)(value >> 16));
        stream.WriteByte((byte)(value >> 8));
        stream.WriteByte((byte)value);
    }

    public static void Insert(string path)
    {
        byte[] png = File.ReadAllBytes(path);
        byte[] signature = { 137, 80, 78, 71, 13, 10, 26, 10 };
        if (png.Length < 33)
            throw new InvalidDataException("PNG is shorter than its signature and IHDR.");
        for (int index = 0; index < signature.Length; index++)
            if (png[index] != signature[index])
                throw new InvalidDataException("Input is not a PNG.");

        byte[] type = Encoding.ASCII.GetBytes("sRGB");
        byte[] crcInput = { type[0], type[1], type[2], type[3], 0 };
        using (var output = new MemoryStream(png.Length + 13))
        {
            output.Write(png, 0, 33);
            WriteBigEndian(output, 1);
            output.Write(type, 0, type.Length);
            output.WriteByte(0);
            WriteBigEndian(output, Crc32(crcInput));
            output.Write(png, 33, png.Length - 33);
            File.WriteAllBytes(path, output.ToArray());
        }
    }
}
'@
    }
    [MondrianPngSrgbChunk]::Insert([IO.Path]::GetFullPath($Path))
}

$ffmpeg = Require-Command "ffmpeg"
$ffprobe = Require-Command "ffprobe"
$ffmpegVersion = ((& $ffmpeg -version 2>&1 | Select-Object -First 1) -join "").Trim()
$ffprobeVersion = ((& $ffprobe -version 2>&1 | Select-Object -First 1) -join "").Trim()
$encoderList = ((& $ffmpeg -hide_banner -encoders 2>&1) -join [Environment]::NewLine)
foreach ($encoder in @("libx265", "png")) {
    if ($encoderList -notmatch "\b$encoder\b") {
        throw "The Golden color-reference recipe requires the FFmpeg '$encoder' encoder."
    }
}
$filterList = ((& $ffmpeg -hide_banner -filters 2>&1) -join [Environment]::NewLine)
foreach ($filter in @("color", "colorchannelmixer", "drawbox", "geq", "zscale")) {
    if ($filterList -notmatch "\b$filter\b") {
        throw "The Golden color-reference recipe requires the FFmpeg '$filter' filter."
    }
}
if ($ValidatePrerequisitesOnly) {
    Write-Host "Golden color-reference media generator prerequisites are available."
    return
}

$outputDirectory = Resolve-RepositoryPath $OutputRoot
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
$recipeHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()

if ($Profile -in @("All", "HlgMain10")) {
    $artifactPath = Join-Path $outputDirectory $HlgFileName
    $shouldGenerate = $Force -or -not (Test-Path -LiteralPath $artifactPath -PathType Leaf)
    if ($shouldGenerate) {
        $partialPath = "$artifactPath.partial.mp4"
        Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
        try {
            # Eight broad patches keep reference samples away from 4:2:0 edges.
            # Values are full-range HLG R'G'B' 10-bit codes before deterministic
            # BT.2020 non-constant-luminance limited-range matrix conversion.
            $red = "if(lt(X,W/8),64,if(lt(X,2*W/8),256,if(lt(X,3*W/8),512,if(lt(X,4*W/8),768,if(lt(X,5*W/8),800,if(lt(X,6*W/8),200,if(lt(X,7*W/8),100,940)))))))"
            $green = "if(lt(X,W/8),64,if(lt(X,2*W/8),256,if(lt(X,3*W/8),512,if(lt(X,4*W/8),768,if(lt(X,5*W/8),200,if(lt(X,6*W/8),800,if(lt(X,7*W/8),200,940)))))))"
            $blue = "if(lt(X,W/8),64,if(lt(X,2*W/8),256,if(lt(X,3*W/8),512,if(lt(X,4*W/8),768,if(lt(X,5*W/8),100,if(lt(X,6*W/8),100,if(lt(X,7*W/8),800,940)))))))"
            $source = "nullsrc=s=1920x1080:r=25:d=1,format=gbrp10le,geq=r='$red':g='$green':b='$blue',zscale=primariesin=2020:transferin=arib-std-b67:matrixin=gbr:rangein=full:primaries=2020:transfer=arib-std-b67:matrix=2020_ncl:range=limited,format=yuv420p10le"
            Invoke-Checked $ffmpeg @(
                "-hide_banner", "-nostdin", "-loglevel", "warning", "-y",
                "-f", "lavfi", "-i", $source,
                "-an",
                "-c:v", "libx265",
                "-preset", "ultrafast",
                "-x265-params", "lossless=1:repeat-headers=1:keyint=25:min-keyint=25:scenecut=0",
                "-tag:v", "hvc1",
                "-color_range", "tv",
                "-colorspace", "bt2020nc",
                "-color_trc", "arib-std-b67",
                "-color_primaries", "bt2020",
                "-movflags", "+faststart",
                $partialPath
            )
            Move-Item -LiteralPath $partialPath -Destination $artifactPath -Force
        } finally {
            Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
        }
    }
    $probe = Read-JsonProbe $ffprobe $artifactPath
    Assert-HlgProbe $probe
    if ($shouldGenerate) {
        Write-Attestation $HlgFixtureId "HlgMain10" $artifactPath $ffmpegVersion $ffprobeVersion $probe
    } else {
        Assert-ExistingAttestation $HlgFixtureId "HlgMain10" $artifactPath $recipeHash
        Write-Host "Reused attested $HlgFixtureId"
    }
}

if ($Profile -in @("All", "SrgbAlpha")) {
    $artifactPath = Join-Path $outputDirectory $SrgbFileName
    $shouldGenerate = $Force -or -not (Test-Path -LiteralPath $artifactPath -PathType Leaf)
    if ($shouldGenerate) {
        $partialPath = "$artifactPath.partial.png"
        Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
        try {
            # The transparent magenta upper half detects accidental use of RGB
            # behind zero coverage. The lower quarters exercise exact straight
            # Alpha coverage at 64, 128, 192, and 255.
            $source = "color=c=black:s=1920x1080:r=1,format=rgba,colorchannelmixer=aa=0,drawbox=x=0:y=0:w=1920:h=540:color=0xFF00FF00:t=fill:replace=1,drawbox=x=0:y=540:w=480:h=540:color=0xFF000040:t=fill:replace=1,drawbox=x=480:y=540:w=480:h=540:color=0x00FF0080:t=fill:replace=1,drawbox=x=960:y=540:w=480:h=540:color=0x0000FFC0:t=fill:replace=1,drawbox=x=1440:y=540:w=480:h=540:color=0xFFFFFFFF:t=fill:replace=1"
            Invoke-Checked $ffmpeg @(
                "-hide_banner", "-nostdin", "-loglevel", "warning", "-y",
                "-f", "lavfi", "-i", $source,
                "-frames:v", "1",
                "-update", "1",
                "-c:v", "png",
                "-pred", "mixed",
                "-pix_fmt", "rgba",
                $partialPath
            )
            Add-PngSrgbChunk $partialPath
            Move-Item -LiteralPath $partialPath -Destination $artifactPath -Force
        } finally {
            Remove-Item -LiteralPath $partialPath -Force -ErrorAction SilentlyContinue
        }
    }
    $probe = Read-JsonProbe $ffprobe $artifactPath
    Assert-SrgbProbe $probe
    if ($shouldGenerate) {
        Write-Attestation $SrgbFixtureId "SrgbAlpha" $artifactPath $ffmpegVersion $ffprobeVersion $probe
    } else {
        Assert-ExistingAttestation $SrgbFixtureId "SrgbAlpha" $artifactPath $recipeHash
        Write-Host "Reused attested $SrgbFixtureId"
    }
}
