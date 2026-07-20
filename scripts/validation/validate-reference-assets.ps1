param(
    [ValidateSet("Pr", "Nightly", "Release")][string]$Tier = "Pr",
    [ValidateSet("All", "Playback")][string]$Scope = "All",
    [string]$FixtureRoot = "tests/fixtures",
    [string]$ContractRoot = "tests/validation",
    [string]$OutputPath = "target/validation/reference-assets.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Add-Issue([string]$Severity, [string]$Code, [string]$Message) {
    $script:issues.Add([pscustomobject]@{ severity = $Severity; code = $Code; message = $Message })
}

function Resolve-ContainedPath([string]$Root, [string]$RelativePath, [string]$Description) {
    if ([IO.Path]::IsPathRooted($RelativePath)) {
        throw "$Description must be relative: $RelativePath"
    }
    $absoluteRoot = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $absolutePath = [IO.Path]::GetFullPath((Join-Path $absoluteRoot $RelativePath))
    $prefix = "$absoluteRoot$([IO.Path]::DirectorySeparatorChar)"
    if (-not $absolutePath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description escapes its declared root: $RelativePath"
    }
    return $absolutePath
}

function Has-Property([object]$Value, [string]$Name) {
    return $null -ne $Value -and $null -ne $Value.PSObject.Properties[$Name]
}

$issues = [System.Collections.Generic.List[object]]::new()
$assetResults = [System.Collections.Generic.List[object]]::new()
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
$fixtureRootAbsolute = if ([IO.Path]::IsPathRooted($FixtureRoot)) { [IO.Path]::GetFullPath($FixtureRoot) } else { [IO.Path]::GetFullPath((Join-Path $repositoryRoot $FixtureRoot)) }
$contractRootAbsolute = if ([IO.Path]::IsPathRooted($ContractRoot)) { [IO.Path]::GetFullPath($ContractRoot) } else { [IO.Path]::GetFullPath((Join-Path $repositoryRoot $ContractRoot)) }
$manifestPath = Join-Path $contractRootAbsolute "corpus-manifest.json"
$goldenPath = Join-Path $contractRootAbsolute "golden-project.json"
$stressPath = Join-Path $contractRootAbsolute "stress-project.json"
$machineProfilePath = Join-Path $contractRootAbsolute "windows-alpha-reference.json"
$playbackPlanPath = Join-Path $contractRootAbsolute "playback-reference-gates.json"
$contractPaths = @($manifestPath, $goldenPath, $stressPath, $machineProfilePath, $playbackPlanPath)

foreach ($path in $contractPaths) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        Add-Issue "error" "contract.missing" "Required contract is missing: $path"
    }
}
if ($issues.Count -gt 0) { throw "Validation contracts are incomplete." }

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$playbackPlan = Get-Content -LiteralPath $playbackPlanPath -Raw | ConvertFrom-Json
$machineProfile = Get-Content -LiteralPath $machineProfilePath -Raw | ConvertFrom-Json
if ($manifest.schema_version -ne 2) { Add-Issue "error" "schema.unsupported" "Unsupported corpus schema version: $($manifest.schema_version)" }
if ($playbackPlan.schema_version -ne 2) { Add-Issue "error" "playback-plan.schema-unsupported" "Unsupported playback gate-plan schema: $($playbackPlan.schema_version)" }
if ($machineProfile.schema_version -ne 3) { Add-Issue "error" "machine-profile.schema-unsupported" "Unsupported Windows machine-profile schema: $($machineProfile.schema_version)" }
if ($playbackPlan.machine_profile -ne $machineProfile.id) { Add-Issue "error" "playback-plan.machine-profile-mismatch" "Playback plan references '$($playbackPlan.machine_profile)' but the configured profile is '$($machineProfile.id)'" }
$diagnosticExecution = if (Has-Property $playbackPlan "diagnostic_execution") { $playbackPlan.diagnostic_execution } else { $null }
$safeDiagnosticMachineIssueCodes = @("machine.logical-cpu", "machine.memory-class")
if ($null -eq $diagnosticExecution) {
    Add-Issue "error" "playback-plan.diagnostic-execution-missing" "Playback plan must define its diagnostic machine-qualification policy"
} else {
    if (-not (Has-Property $diagnosticExecution "explicit_unqualified_machine_opt_in_required") -or $diagnosticExecution.explicit_unqualified_machine_opt_in_required -ne $true) {
        Add-Issue "error" "playback-plan.diagnostic-opt-in-required" "Unqualified-machine diagnostics must require explicit operator opt-in"
    }
    if (-not (Has-Property $diagnosticExecution "waivable_machine_issue_codes")) {
        Add-Issue "error" "playback-plan.diagnostic-waivers-missing" "Playback plan must explicitly list diagnostic-waivable machine issues"
    } else {
        $diagnosticWaivers = @($diagnosticExecution.waivable_machine_issue_codes | ForEach-Object { [string]$_ })
        foreach ($code in $diagnosticWaivers) {
            if ([string]::IsNullOrWhiteSpace($code)) {
                Add-Issue "error" "playback-plan.diagnostic-waiver-empty" "Diagnostic machine issue codes must be non-empty"
            } elseif ($code -notin $safeDiagnosticMachineIssueCodes) {
                Add-Issue "error" "playback-plan.diagnostic-waiver-unsafe" "Machine issue '$code' is an execution prerequisite and cannot be waived for diagnostics"
            }
        }
        if (@($diagnosticWaivers | Select-Object -Unique).Count -ne $diagnosticWaivers.Count) {
            Add-Issue "error" "playback-plan.diagnostic-waiver-duplicate" "Diagnostic machine issue codes must be unique"
        }
    }
}
$supportedMemoryClasses = @("minimum-supported", "standard-playback", "professional-large-project")
if (-not (Has-Property $playbackPlan "baseline_machine_requirements") -or -not (Has-Property $playbackPlan.baseline_machine_requirements "memory_class")) {
    Add-Issue "error" "playback-plan.baseline-memory-class-missing" "Playback plan must declare the memory class required for baseline evidence"
} elseif ([string]$playbackPlan.baseline_machine_requirements.memory_class -notin $supportedMemoryClasses) {
    Add-Issue "error" "playback-plan.baseline-memory-class-unknown" "Playback plan declares an unknown baseline memory class"
}
$memoryClassContract = if (Has-Property $machineProfile.hardware "memory_classes_gib") { $machineProfile.hardware.memory_classes_gib } else { $null }
if ($null -eq $memoryClassContract) {
    Add-Issue "error" "machine-profile.memory-classes-missing" "Windows machine profile must define memory support classes"
} else {
    $minimumMemory = [int]$memoryClassContract.'minimum-supported'
    $standardMemory = [int]$memoryClassContract.'standard-playback'
    $professionalMemory = [int]$memoryClassContract.'professional-large-project'
    if ($minimumMemory -ne 8 -or $minimumMemory -ge $standardMemory -or $standardMemory -ge $professionalMemory) {
        Add-Issue "error" "machine-profile.memory-classes-invalid" "Memory classes must start at the 8 GiB support floor and increase from minimum to standard to professional"
    }
}
$playbackFixtureIds = @($playbackPlan.gates | ForEach-Object { [string]$_.fixture_id })

$ids = @{}
$requiredFields = @("id", "path", "availability", "sha256", "size_bytes", "provenance", "media", "purposes")
foreach ($entry in $manifest.entries) {
    $entryId = if (Has-Property $entry "id") { [string]$entry.id } else { "<unknown>" }
    $missingEntryFields = $false
    foreach ($field in $requiredFields) {
        if (-not (Has-Property $entry $field)) {
            Add-Issue "error" "entry.field-missing" "${entryId}: missing $field"
            $missingEntryFields = $true
        }
    }
    if ($missingEntryFields) { continue }
    if ($ids.ContainsKey($entryId)) { Add-Issue "error" "entry.duplicate-id" "Duplicate fixture id: $entryId" } else { $ids[$entryId] = $entry }
    if ($entry.availability -notin @("committed", "local-restricted", "generated")) { Add-Issue "error" "entry.bad-availability" "${entryId}: invalid availability" }
    if (-not (Has-Property $entry.provenance "source") -or [string]::IsNullOrWhiteSpace([string]$entry.provenance.source)) { Add-Issue "error" "entry.provenance-source-missing" "${entryId}: provenance source is required" }
    if (-not (Has-Property $entry.provenance "rights_basis") -or [string]::IsNullOrWhiteSpace([string]$entry.provenance.rights_basis)) { Add-Issue "error" "entry.rights-basis-missing" "${entryId}: an explicit rights basis is required" }
    if (-not (Has-Property $entry.provenance "redistribution") -or $entry.provenance.redistribution -notin @("permitted", "prohibited")) { Add-Issue "error" "entry.unverified-provenance" "${entryId}: usage and redistribution rights are not explicit" }
    if ((Has-Property $entry.media "color_reference_eligible") -and $entry.media.color_reference_eligible -eq $false -and @($entry.purposes | Where-Object { $_ -match "color" }).Count -gt 0) { Add-Issue "error" "entry.ineligible-color-purpose" "${entryId}: a color-ineligible fixture declares a color purpose" }

    $artifactPath = $null
    try {
        $artifactPath = Resolve-ContainedPath $fixtureRootAbsolute ([string]$entry.path) "Fixture path"
    } catch {
        Add-Issue "error" "entry.path-invalid" "${entryId}: $($_.Exception.Message)"
    }

    $fixedIdentity = $entry.availability -in @("committed", "local-restricted")
    $generationContractUsable = $false
    if ($fixedIdentity) {
        if ($entry.sha256 -notmatch '^[0-9a-f]{64}$') { Add-Issue "error" "entry.bad-hash" "${entryId}: fixed artifacts require lowercase SHA-256" }
        if ($null -eq $entry.size_bytes -or [int64]$entry.size_bytes -lt 0) { Add-Issue "error" "entry.bad-size" "${entryId}: fixed artifacts require a non-negative byte size" }
        if (Has-Property $entry "generation") { Add-Issue "error" "entry.unexpected-generation" "${entryId}: fixed artifacts cannot declare a generation recipe" }
    } elseif ($entry.availability -eq "generated") {
        if ($null -ne $entry.sha256 -or $null -ne $entry.size_bytes) { Add-Issue "error" "entry.generated-global-identity" "${entryId}: generated output hash and size belong to each run, not the corpus manifest" }
        if (-not (Has-Property $entry "generation")) {
            Add-Issue "error" "entry.generation-missing" "${entryId}: generated fixture has no recipe contract"
        } else {
            $generation = $entry.generation
            $missingGenerationFields = @("recipe_path", "recipe_sha256", "profile", "attestation_suffix", "artifact_hash_scope") | Where-Object { -not (Has-Property $generation $_) }
            foreach ($field in $missingGenerationFields) { Add-Issue "error" "entry.generation-field-missing" "${entryId}: generation contract is missing $field" }
            if (@($missingGenerationFields).Count -eq 0) {
                $generationContractUsable = $true
                if ($generation.recipe_sha256 -notmatch '^[0-9a-f]{64}$') { Add-Issue "error" "entry.recipe-bad-hash" "${entryId}: recipe SHA-256 must be lowercase hexadecimal" }
                if ($generation.artifact_hash_scope -ne "reference-run") { Add-Issue "error" "entry.artifact-hash-scope" "${entryId}: generated artifacts must be pinned by each reference run" }
                try {
                    $recipePath = Resolve-ContainedPath $repositoryRoot ([string]$generation.recipe_path) "Recipe path"
                    if (-not (Test-Path -LiteralPath $recipePath -PathType Leaf)) {
                        Add-Issue "error" "entry.recipe-missing" "${entryId}: recipe is absent at $recipePath"
                    } else {
                        $recipeHash = (Get-FileHash -LiteralPath $recipePath -Algorithm SHA256).Hash.ToLowerInvariant()
                        if ($recipeHash -ne $generation.recipe_sha256) { Add-Issue "error" "entry.recipe-hash-mismatch" "${entryId}: recipe SHA-256 differs from the manifest" }
                    }
                } catch {
                    Add-Issue "error" "entry.recipe-path-invalid" "${entryId}: $($_.Exception.Message)"
                }
            }
        }
    }

    $mustExist = $entry.availability -eq "committed" -or ($Tier -in @("Nightly", "Release") -and ($Scope -eq "All" -or $entryId -in $playbackFixtureIds))
    $artifactResult = [ordered]@{
        fixture_id = $entryId
        availability = $entry.availability
        path = if ($null -eq $artifactPath) { $null } else { $artifactPath }
        present = $false
        size_bytes = $null
        sha256 = $null
        attested = $false
    }
    if ($null -ne $artifactPath -and (Test-Path -LiteralPath $artifactPath -PathType Leaf)) {
        $item = Get-Item -LiteralPath $artifactPath
        $actualHash = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
        $artifactResult.present = $true
        $artifactResult.size_bytes = [int64]$item.Length
        $artifactResult.sha256 = $actualHash
        if ($fixedIdentity) {
            if ($item.Length -ne [int64]$entry.size_bytes) { Add-Issue "error" "asset.size-mismatch" "${entryId}: expected $($entry.size_bytes) bytes, got $($item.Length)" }
            if ($actualHash -ne $entry.sha256) { Add-Issue "error" "asset.hash-mismatch" "${entryId}: SHA-256 differs from the manifest" }
        } elseif ($generationContractUsable) {
            $attestationPath = "$artifactPath$($entry.generation.attestation_suffix)"
            if (-not (Test-Path -LiteralPath $attestationPath -PathType Leaf)) {
                Add-Issue "error" "asset.attestation-missing" "${entryId}: generated artifact has no attestation at $attestationPath"
            } else {
                try {
                    $attestation = Get-Content -LiteralPath $attestationPath -Raw | ConvertFrom-Json
                    $attestationValid = $attestation.schema_version -eq 1 -and $attestation.fixture_id -eq $entryId -and $attestation.recipe.sha256 -eq $entry.generation.recipe_sha256 -and $attestation.artifact.sha256 -eq $actualHash -and [int64]$attestation.artifact.size_bytes -eq [int64]$item.Length
                    if (-not $attestationValid) { Add-Issue "error" "asset.attestation-mismatch" "${entryId}: generated attestation does not bind this recipe and artifact" } else { $artifactResult.attested = $true }
                } catch {
                    Add-Issue "error" "asset.attestation-invalid" "${entryId}: cannot read generated attestation: $($_.Exception.Message)"
                }
            }
        }
    } elseif ($mustExist) {
        Add-Issue "blocked" "asset.missing" "${entryId}: required asset is absent at $artifactPath"
    }
    $assetResults.Add([pscustomobject]$artifactResult)
}

$golden = Get-Content -LiteralPath $goldenPath -Raw | ConvertFrom-Json
$stress = Get-Content -LiteralPath $stressPath -Raw | ConvertFrom-Json
foreach ($project in @($golden, $stress)) {
    if ($project.schema_version -ne 1) { Add-Issue "error" "project.schema-unsupported" "$($project.id) has an unsupported schema version" }
    if ($project.kind -notin @("golden", "stress")) { Add-Issue "error" "project.bad-kind" "$($project.id) has an invalid project kind" }
}

$gateIds = @{}
foreach ($gate in $playbackPlan.gates) {
    $missingGateFields = @("id", "fixture_id", "required_purposes", "cargo_test", "media_environment", "expected_report_profile", "expected_report_path") | Where-Object { -not (Has-Property $gate $_) }
    foreach ($field in $missingGateFields) { Add-Issue "error" "playback-plan.gate-field-missing" "Playback gate is missing '$field'" }
    if (@($missingGateFields).Count -gt 0) { continue }
    if ($gateIds.ContainsKey($gate.id)) { Add-Issue "error" "playback-plan.duplicate-gate" "Duplicate playback gate id: $($gate.id)" } else { $gateIds[$gate.id] = $true }
    if (-not $ids.ContainsKey($gate.fixture_id)) {
        Add-Issue "error" "playback-plan.fixture-unknown" "Gate '$($gate.id)' references unknown fixture '$($gate.fixture_id)'"
        continue
    }
    $fixture = $ids[$gate.fixture_id]
    foreach ($field in @("cargo_test", "media_environment", "expected_report_profile", "expected_report_path")) {
        if (-not (Has-Property $gate $field) -or [string]::IsNullOrWhiteSpace([string]$gate.$field)) { Add-Issue "error" "playback-plan.gate-field-missing" "Gate '$($gate.id)' requires non-empty '$field'" }
    }
    foreach ($purpose in $gate.required_purposes) {
        if ($purpose -notin @($fixture.purposes)) { Add-Issue "error" "playback-plan.purpose-missing" "Gate '$($gate.id)' requires purpose '$purpose' on fixture '$($fixture.id)'" }
    }
}
foreach ($gateId in $playbackPlan.baseline_acceptance.required_gate_ids) {
    if (-not $gateIds.ContainsKey($gateId)) { Add-Issue "error" "playback-plan.required-gate-missing" "Baseline requires unknown gate '$gateId'" }
}

if ($Tier -in @("Nightly", "Release") -and $Scope -eq "All") {
    foreach ($role in $golden.required_fixture_roles) {
        if ($role.required -and [string]::IsNullOrWhiteSpace($role.fixture_id)) {
            Add-Issue "blocked" "golden.fixture-unassigned" "Golden Project role '$($role.role)' has no verified fixture assigned"
        } elseif (-not [string]::IsNullOrWhiteSpace($role.fixture_id) -and -not $ids.ContainsKey($role.fixture_id)) {
            Add-Issue "error" "golden.fixture-unknown" "Golden Project role '$($role.role)' references unknown fixture '$($role.fixture_id)'"
        }
    }
    foreach ($purpose in $stress.required_fixture_purposes) {
        $matchingFixtures = @($manifest.entries | Where-Object { $purpose -in @($_.purposes) })
        if ($matchingFixtures.Count -eq 0) { Add-Issue "blocked" "stress.fixture-purpose-unassigned" "Stress Project purpose '$purpose' has no verified fixture" }
    }
}

$errors = @($issues | Where-Object severity -eq "error")
$blocked = @($issues | Where-Object severity -eq "blocked")
$status = if ($errors.Count -gt 0) { "failed" } elseif ($blocked.Count -gt 0) { "blocked" } else { "passed" }
$report = [ordered]@{
    schema_version = 2
    tier = $Tier
    scope = $Scope
    status = $status
    corpus_revision = $manifest.corpus_revision
    checked_at_utc = [DateTime]::UtcNow.ToString("o")
    entry_count = @($manifest.entries).Count
    assets = @($assetResults)
    issues = @($issues)
}

$absoluteOutputPath = if ([IO.Path]::IsPathRooted($OutputPath)) { [IO.Path]::GetFullPath($OutputPath) } else { [IO.Path]::GetFullPath((Join-Path $repositoryRoot $OutputPath)) }
$parent = Split-Path -Parent $absoluteOutputPath
if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $absoluteOutputPath -Encoding utf8
Write-Host "Reference asset validation: $status ($Tier/$Scope); report: $absoluteOutputPath"
foreach ($issue in $issues) { Write-Host "[$($issue.severity)] $($issue.code): $($issue.message)" }
if ($status -ne "passed") { exit 1 }
