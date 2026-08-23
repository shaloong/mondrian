[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$PackagePath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ManifestPath,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-fA-F]{40}$')]
    [string]$ExpectedSourceSha,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ExpectedRepository,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 32)]
    [int]$PassIndex,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-RepositoryPath([string]$Path) {
    if ([IO.Path]::IsPathRooted($Path)) { return [IO.Path]::GetFullPath($Path) }
    return [IO.Path]::GetFullPath((Join-Path $script:repositoryRoot $Path))
}

function Read-JsonObject([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing: $Path"
    }
    try {
        return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    } catch {
        throw "$Label is not valid JSON: $($_.Exception.Message)"
    }
}

if (-not $IsWindows) {
    throw 'Windows candidate-package qualification must execute on Windows.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$contractPath = Resolve-RepositoryPath 'tests/validation/windows-candidate-package.json'
$package = Resolve-RepositoryPath $PackagePath
$manifestFile = Resolve-RepositoryPath $ManifestPath
$evidenceFile = Resolve-RepositoryPath $OutputPath
$sourceSha = $ExpectedSourceSha.ToLowerInvariant()
$contract = Read-JsonObject $contractPath 'Windows candidate-package contract'
$manifest = Read-JsonObject $manifestFile 'Candidate-package manifest'

$headSha = ([string](& git -C $repositoryRoot rev-parse HEAD)).Trim().ToLowerInvariant()
if ($LASTEXITCODE -ne 0 -or $headSha -ne $sourceSha) {
    throw "Checked-out source SHA '$headSha' does not match expected SHA '$sourceSha'."
}

$contractHash = (Get-FileHash -LiteralPath $contractPath -Algorithm SHA256).Hash.ToLowerInvariant()
$packageItem = Get-Item -LiteralPath $package -ErrorAction Stop
$packageHash = (Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash.ToLowerInvariant()
if (
    $contract.schema_version -ne 1 -or
    $contract.id -ne 'windows-portable-candidate-v1' -or
    $contract.runner -ne 'windows-2022' -or
    $contract.required_independent_passes -ne 3 -or
    $contract.package.format -ne 'zip' -or
    $contract.runtime_verification.executable -ne 'mondrian.exe' -or
    $contract.runtime_verification.isolated_user_state_required -ne $true -or
    $contract.runtime_verification.sanitized_path_required -ne $true -or
    $contract.runtime_verification.proxy_denied_required -ne $true
) {
    throw 'Windows candidate-package contract is unsupported or incomplete.'
}
if ($PassIndex -gt $contract.required_independent_passes) {
    throw "Pass index $PassIndex exceeds the contract pass count."
}
if (
    $manifest.schema_version -ne 1 -or
    $manifest.contract.id -ne $contract.id -or
    $manifest.contract.sha256 -ne $contractHash -or
    $manifest.repository -ne $ExpectedRepository -or
    $manifest.source_sha -ne $sourceSha -or
    $manifest.package.file_name -ne $packageItem.Name -or
    $manifest.package.sha256 -ne $packageHash -or
    $manifest.package.size_bytes -ne $packageItem.Length -or
    $manifest.status -ne 'built'
) {
    throw 'Candidate-package manifest does not match the contract, source, repository, or package bytes.'
}

$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$runRoot = [IO.Path]::GetFullPath(
    (Join-Path $temporaryRoot "mondrian-candidate-$([Guid]::NewGuid().ToString('N'))")
)
$temporaryPrefix = $temporaryRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if (-not $runRoot.StartsWith($temporaryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Candidate extraction root escaped the system temporary directory: $runRoot"
}
$extractRoot = Join-Path $runRoot 'package'
$userRoot = Join-Path $runRoot 'user'
$localState = Join-Path $userRoot 'AppData\Local'
$roamingState = Join-Path $userRoot 'AppData\Roaming'
$temporaryState = Join-Path $runRoot 'temp'
$savedEnvironment = @{}
$isolatedVariables = @('APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'TEMP', 'TMP', 'PATH', 'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY')
foreach ($name in $isolatedVariables) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

try {
    New-Item -ItemType Directory -Path $extractRoot, $localState, $roamingState, $temporaryState -Force | Out-Null
    Expand-Archive -LiteralPath $package -DestinationPath $extractRoot -Force

    foreach ($relativePath in @($contract.package.required_root_files)) {
        $requiredPath = Join-Path $extractRoot ([string]$relativePath)
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            throw "Candidate package is missing required root file '$relativePath'."
        }
    }

    $provenancePath = Join-Path $extractRoot 'RELEASE_PROVENANCE.json'
    $provenance = Read-JsonObject $provenancePath 'Packaged release provenance'
    if (
        $provenance.schema_version -ne 1 -or
        $provenance.repository -ne $ExpectedRepository -or
        $provenance.source_sha -ne $sourceSha -or
        $provenance.commercial_engine_qualification.contract -ne 'windows-commercial-engine-v1' -or
        [string]::IsNullOrWhiteSpace([string]$provenance.trusted_ci.run_id) -or
        [string]::IsNullOrWhiteSpace([string]$provenance.commercial_engine_qualification.run_id)
    ) {
        throw 'Packaged release provenance is not bound to the qualified source and engine evidence.'
    }

    $env:APPDATA = $roamingState
    $env:LOCALAPPDATA = $localState
    $env:USERPROFILE = $userRoot
    $env:TEMP = $temporaryState
    $env:TMP = $temporaryState
    $env:PATH = "$extractRoot;$env:SystemRoot\System32;$env:SystemRoot"
    $env:HTTP_PROXY = 'http://127.0.0.1:9'
    $env:HTTPS_PROXY = 'http://127.0.0.1:9'
    $env:ALL_PROXY = 'http://127.0.0.1:9'
    $env:NO_PROXY = ''

    $executable = Join-Path $extractRoot ([string]$contract.runtime_verification.executable)
    $arguments = @($contract.runtime_verification.arguments | ForEach-Object { [string]$_ })
    Push-Location $extractRoot
    try {
        & $executable @arguments
        $runtimeExitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($runtimeExitCode -ne 0) {
        throw "Candidate runtime verification failed with exit code $runtimeExitCode."
    }

    $evidence = [ordered]@{
        schema_version = 1
        contract = [ordered]@{
            id = [string]$contract.id
            sha256 = $contractHash
        }
        repository = $ExpectedRepository
        source_sha = $sourceSha
        package = [ordered]@{
            file_name = $packageItem.Name
            sha256 = $packageHash
            size_bytes = $packageItem.Length
        }
        pass = [ordered]@{
            index = $PassIndex
            required_independent_passes = [int]$contract.required_independent_passes
            workflow_run_id = [string]$env:GITHUB_RUN_ID
            workflow_run_attempt = [string]$env:GITHUB_RUN_ATTEMPT
            runner_name = [string]$env:RUNNER_NAME
            runner_environment = [string]$env:RUNNER_ENVIRONMENT
            runner_image = [string]$env:ImageOS
            runner_image_version = [string]$env:ImageVersion
        }
        execution = [ordered]@{
            executable = [string]$contract.runtime_verification.executable
            arguments = $arguments
            exit_code = $runtimeExitCode
            isolated_user_state = $true
            sanitized_path = $true
            proxy_denied = $true
        }
        generated_at_utc = [DateTime]::UtcNow.ToString('o')
        status = 'passed'
    }
    $evidenceDirectory = Split-Path -Parent $evidenceFile
    New-Item -ItemType Directory -Path $evidenceDirectory -Force | Out-Null
    $evidence | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $evidenceFile -Encoding utf8
} finally {
    foreach ($name in $isolatedVariables) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
    }
    if (Test-Path -LiteralPath $runRoot) {
        Remove-Item -LiteralPath $runRoot -Recurse -Force
    }
}

Write-Host "Windows candidate package pass $PassIndex/$($contract.required_independent_passes): passed ($packageHash)"
