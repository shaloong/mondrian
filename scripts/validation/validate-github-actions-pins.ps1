[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$workflowRoot = Join-Path $repositoryRoot '.github\workflows'
$violations = [System.Collections.Generic.List[string]]::new()

Get-ChildItem -LiteralPath $workflowRoot -File -Include '*.yml', '*.yaml' | ForEach-Object {
    $workflow = $_
    $lineNumber = 0

    Get-Content -LiteralPath $workflow.FullName | ForEach-Object {
        $lineNumber++
        if ($_ -notmatch '^\s*(?:-\s*)?uses:\s*(?<reference>[^\s#]+)') {
            return
        }

        $reference = $Matches.reference
        if ($reference.StartsWith('./') -or $reference.StartsWith('docker://')) {
            return
        }

        if ($reference -notmatch '^[^/@]+/[^@]+@(?<revision>[0-9a-f]{40})$') {
            $relativePath = [System.IO.Path]::GetRelativePath($repositoryRoot, $workflow.FullName)
            $violations.Add("${relativePath}:${lineNumber}: '$reference' must use a full 40-character lowercase commit SHA.")
        }
    }
}

if ($violations.Count -gt 0) {
    $violations | ForEach-Object { Write-Error $_ }
    throw "GitHub Actions pin validation failed with $($violations.Count) violation(s)."
}

Write-Host 'GitHub Actions pin validation passed.'
