param(
    [ValidateSet("Pr", "Nightly", "Release")][string]$Tier = "Pr",
    [string]$FixtureRoot = "tests/fixtures",
    [string]$ContractRoot = "tests/validation",
    [string]$OutputPath = "target/validation/reference-assets.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Add-Issue([string]$Severity, [string]$Code, [string]$Message) {
    $script:issues.Add([pscustomobject]@{ severity = $Severity; code = $Code; message = $Message })
}

$issues = [System.Collections.Generic.List[object]]::new()
$manifestPath = Join-Path $ContractRoot "corpus-manifest.json"
$projectPaths = @("golden-project.json", "stress-project.json", "windows-alpha-reference.json") | ForEach-Object { Join-Path $ContractRoot $_ }

foreach ($path in @($manifestPath) + $projectPaths) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Add-Issue "error" "contract.missing" "Required contract is missing: $path"
    }
}
if ($issues.Count -gt 0) { throw "Validation contracts are incomplete." }

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.schema_version -ne 1) { Add-Issue "error" "schema.unsupported" "Unsupported corpus schema version: $($manifest.schema_version)" }

$ids = @{}
$requiredFields = @("id", "path", "availability", "sha256", "size_bytes", "provenance", "media", "purposes")
foreach ($entry in $manifest.entries) {
    foreach ($field in $requiredFields) {
        if ($null -eq $entry.PSObject.Properties[$field]) { Add-Issue "error" "entry.field-missing" "$($entry.id): missing $field" }
    }
    if ($ids.ContainsKey($entry.id)) { Add-Issue "error" "entry.duplicate-id" "Duplicate fixture id: $($entry.id)" } else { $ids[$entry.id] = $true }
    if ($entry.sha256 -notmatch '^[0-9a-f]{64}$') { Add-Issue "error" "entry.bad-hash" "$($entry.id): SHA-256 must be lowercase hexadecimal" }
    if ($entry.availability -notin @("committed", "local-restricted", "generated")) { Add-Issue "error" "entry.bad-availability" "$($entry.id): invalid availability" }
    if ($entry.provenance.redistribution -notin @("permitted", "prohibited")) { Add-Issue "error" "entry.unverified-provenance" "$($entry.id): canonical fixtures require verified usage and redistribution rights" }

    $path = Join-Path $FixtureRoot $entry.path
    $mustExist = $entry.availability -eq "committed" -or $Tier -in @("Nightly", "Release")
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        if ($mustExist) { Add-Issue "blocked" "asset.missing" "$($entry.id): required asset is absent at $path" }
        continue
    }
    $item = Get-Item -LiteralPath $path
    if ($item.Length -ne [int64]$entry.size_bytes) { Add-Issue "error" "asset.size-mismatch" "$($entry.id): expected $($entry.size_bytes) bytes, got $($item.Length)"; continue }
    $actualHash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $entry.sha256) { Add-Issue "error" "asset.hash-mismatch" "$($entry.id): SHA-256 differs from the manifest" }
}

foreach ($projectPath in $projectPaths[0..1]) {
    $project = Get-Content -LiteralPath $projectPath -Raw | ConvertFrom-Json
    if ($project.schema_version -ne 1) { Add-Issue "error" "project.schema-unsupported" "$projectPath has an unsupported schema version" }
    if ($project.kind -notin @("golden", "stress")) { Add-Issue "error" "project.bad-kind" "$projectPath has an invalid project kind" }
}

if ($Tier -in @("Nightly", "Release")) {
    $golden = Get-Content -LiteralPath $projectPaths[0] -Raw | ConvertFrom-Json
    foreach ($role in $golden.required_fixture_roles) {
        if ($role.required -and [string]::IsNullOrWhiteSpace($role.fixture_id)) {
            Add-Issue "blocked" "golden.fixture-unassigned" "Golden Project role '$($role.role)' has no verified fixture assigned"
        } elseif (-not [string]::IsNullOrWhiteSpace($role.fixture_id) -and -not $ids.ContainsKey($role.fixture_id)) {
            Add-Issue "error" "golden.fixture-unknown" "Golden Project role '$($role.role)' references unknown fixture '$($role.fixture_id)'"
        }
    }
}

$errors = @($issues | Where-Object severity -eq "error")
$blocked = @($issues | Where-Object severity -eq "blocked")
$status = if ($errors.Count -gt 0) { "failed" } elseif ($blocked.Count -gt 0) { "blocked" } else { "passed" }
$report = [ordered]@{ schema_version = 1; tier = $Tier; status = $status; corpus_revision = $manifest.corpus_revision; checked_at_utc = [DateTime]::UtcNow.ToString("o"); entry_count = @($manifest.entries).Count; issues = @($issues) }

$parent = Split-Path -Parent $OutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
Write-Host "Reference asset validation: $status ($Tier); report: $OutputPath"
foreach ($issue in $issues) { Write-Host "[$($issue.severity)] $($issue.code): $($issue.message)" }
if ($status -ne "passed") { exit 1 }
