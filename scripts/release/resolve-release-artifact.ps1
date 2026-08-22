[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [AllowEmptyString()]
    [string]$SourceRef,

    [Parameter(Mandatory = $true)]
    [AllowEmptyString()]
    [string]$SourceRefName,

    [Parameter(Mandatory = $true)]
    [AllowEmptyString()]
    [string]$RequestedTag,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [int]::MaxValue)]
    [int]$RunNumber,

    [Parameter(Mandatory = $true)]
    [ValidateSet('Windows', 'Linux', 'macOS')]
    [string]$RunnerOs,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9_-]*$')]
    [string]$Target,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]*$')]
    [string]$BinName,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]*$')]
    [string]$ArtifactPlatform,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]*$')]
    [string]$ArtifactArch,

    [Parameter(Mandatory = $true)]
    [ValidateSet('zip', 'tar.gz')]
    [string]$Extension,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RepositoryRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$GitHubEnvironmentPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$GitHubOutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$semanticVersionPattern = '^v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-(?<prerelease>[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
if ($SourceRef.StartsWith('refs/tags/', [StringComparison]::Ordinal)) {
    $version = $SourceRefName
} elseif (-not [string]::IsNullOrWhiteSpace($RequestedTag)) {
    $version = $RequestedTag
} else {
    $version = "manual-$RunNumber"
}

if ($version -ne "manual-$RunNumber") {
    $semanticVersion = [regex]::Match($version, $semanticVersionPattern)
    if (-not $semanticVersion.Success) {
        throw "Release version '$version' is not a strict v<major>.<minor>.<patch> SemVer tag."
    }
    if ($semanticVersion.Groups['prerelease'].Success) {
        foreach ($identifier in $semanticVersion.Groups['prerelease'].Value.Split('.')) {
            if ($identifier -match '^[0-9]+$' -and $identifier.Length -gt 1 -and $identifier.StartsWith('0')) {
                throw "Release version '$version' has a numeric prerelease identifier with a leading zero."
            }
        }
    }
}
if ($version -notmatch '^[0-9A-Za-z][0-9A-Za-z.+-]*$') {
    throw "Release version '$version' contains characters that are unsafe for artifact paths."
}

$repository = [IO.Path]::GetFullPath($RepositoryRoot)
$repositoryPrefix = $repository.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$binaryFile = if ($RunnerOs -eq 'Windows') { "$BinName.exe" } else { $BinName }
$binaryRelativePath = Join-Path "target/$Target/release" $binaryFile
$packageName = "$BinName-$version-$ArtifactPlatform-$ArtifactArch"
$packageRelativePath = "$packageName.$Extension"

foreach ($candidate in @($binaryRelativePath, $packageRelativePath, $packageName)) {
    $resolved = [IO.Path]::GetFullPath((Join-Path $repository $candidate))
    if (-not $resolved.StartsWith($repositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Resolved release path escaped the repository root: $candidate"
    }
}

@(
    "RELEASE_VERSION=$version"
    "BIN_PATH=$binaryRelativePath"
    "PACKAGE_NAME=$packageName"
    "PACKAGE_PATH=$packageRelativePath"
) | Out-File -FilePath $GitHubEnvironmentPath -Encoding utf8 -Append

@(
    "release_version=$version"
    "package_name=$packageName"
    "package_path=$packageRelativePath"
) | Out-File -FilePath $GitHubOutputPath -Encoding utf8 -Append

Write-Host "Resolved release artifact '$packageRelativePath' inside '$repository'."
