[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$failures = [System.Collections.Generic.List[string]]::new()
$required = @(
    'README.md', 'CODE_OF_CONDUCT.md', 'GOVERNANCE.md', 'SUPPORT.md',
    '.github/SECURITY.md', '.github/PULL_REQUEST_TEMPLATE.md',
    '.github/ISSUE_TEMPLATE/bug_report.yml', '.github/ISSUE_TEMPLATE/feature_request.yml',
    '.github/ISSUE_TEMPLATE/config.yml', '.github/dependabot.yml',
    '.github/workflows/dependency-audit.yml', 'docs/CONTRIBUTING.md',
    'docs/README.md', 'docs/legal/CLA-SERVICE.md', 'docs/dev/code-review.md',
    'docs/security/design.md', 'docs/security/dependencies.md'
)

function Test-RepositoryReference([string]$sourcePath, [string]$reference) {
    $target = $reference.Trim('<', '>').Split('#')[0]
    if ([string]::IsNullOrWhiteSpace($target) -or $target -match '^[a-zA-Z][a-zA-Z0-9+.-]*:') {
        return
    }
    $target = [Uri]::UnescapeDataString($target)
    $fullPath = [IO.Path]::GetFullPath((Join-Path (Split-Path $sourcePath -Parent) $target))
    $rootPrefix = $repositoryRoot + [IO.Path]::DirectorySeparatorChar
    if (-not $fullPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        $failures.Add("Reference outside repository: $sourcePath -> $reference")
    } elseif (-not (Test-Path -LiteralPath $fullPath)) {
        $failures.Add("Missing reference: $sourcePath -> $reference")
    }
}

foreach ($relativePath in $required) {
    $fullPath = Join-Path $repositoryRoot $relativePath
    if (-not (Test-Path -LiteralPath $fullPath -PathType Leaf)) {
        $failures.Add("Missing required community file: $relativePath")
        continue
    }
    if ($relativePath.EndsWith('.md')) {
        $content = Get-Content -Raw -LiteralPath $fullPath
        foreach ($link in [regex]::Matches($content, '\[[^\]\r\n]*\]\(([^\)\r\n]+)\)')) {
            Test-RepositoryReference $fullPath $link.Groups[1].Value
        }
        if ($content.Contains('https://github.com/mondrian-studio/mondrian')) {
            $failures.Add("Stale repository URL: $relativePath")
        }
    }
}

$workflowRoot = Join-Path $repositoryRoot '.github/workflows'
foreach ($workflow in Get-ChildItem -LiteralPath $workflowRoot -File | Where-Object { $_.Extension -in @('.yml', '.yaml') }) {
    $content = Get-Content -Raw -LiteralPath $workflow.FullName
    if ($content -notmatch '(?m)^permissions:\r?\n  contents: read\s*$') {
        $failures.Add("Workflow must default to contents: read: $($workflow.Name)")
    }
    # Verify checkout's own with block, not an unrelated step's setting.
    $lines = @(Get-Content -LiteralPath $workflow.FullName)
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^(?<indent>[ ]*)- uses: actions/checkout@') {
            $indent = $Matches.indent
            $setting = "${indent}    persist-credentials: false"
            if ($i + 2 -ge $lines.Count -or $lines[$i + 1] -ne "${indent}  with:" -or $lines[$i + 2] -ne $setting) {
                $failures.Add("Checkout must disable persisted credentials: $($workflow.Name):$($i + 1)")
            }
        }
    }
}

if ($failures.Count -gt 0) {
    throw ($failures -join [Environment]::NewLine)
}
& (Join-Path $PSScriptRoot 'validate-github-actions-pins.ps1')
Write-Host 'Community standards validation passed.'
