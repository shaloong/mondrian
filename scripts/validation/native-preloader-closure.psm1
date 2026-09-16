Set-StrictMode -Version Latest

function Assert-PreloaderFields($Value, [string[]]$Fields, [string]$Label) {
    if ($null -eq $Value) { throw "$Label is absent." }
    $actual = @($Value.PSObject.Properties.Name)
    if ($actual.Count -ne $Fields.Count -or @($actual | Where-Object { $_ -cnotin $Fields }).Count -ne 0) {
        throw "$Label has missing or unknown raw fields."
    }
}

function Assert-PreloaderInteger($Value, [uint64]$Maximum, [string]$Label) {
    if (($Value -isnot [int] -and $Value -isnot [long] -and $Value -isnot [uint32] -and $Value -isnot [uint64]) -or
        $Value -lt 0 -or [uint64]$Value -gt $Maximum) { throw "$Label is not one bounded native integer." }
}
function Assert-NativePreloaderReport {
    param($Report, [string]$ManifestPath, [string]$ManifestSha256, $MachinePlan,
        [string]$MachinePlanSha256, [string]$RuntimeImageSha256)
    Assert-PreloaderFields $Report @('schema_version','attestation','child_manifest','exit_code','deadline_exceeded','capsule_removed','descendants_reaped','errors') 'Native pre-loader report'
    Assert-PreloaderInteger $Report.schema_version 1 'Pre-loader schema'
    Assert-PreloaderInteger $Report.exit_code 0 'Native exit code'
    if ($Report.schema_version -cne 1 -or $Report.exit_code -cne 0 -or
        $Report.deadline_exceeded -isnot [bool] -or $Report.deadline_exceeded -or
        $Report.capsule_removed -isnot [bool] -or -not $Report.capsule_removed -or
        $Report.descendants_reaped -isnot [bool] -or -not $Report.descendants_reaped -or
        $Report.errors -isnot [System.Array] -or $Report.errors.Count -ne 0) {
        throw 'Native pre-loader did not close all native owners and its exact namespace.'
    }
    Assert-PreloaderFields $Report.child_manifest @('path','sha256') 'Pre-loader child manifest binding'
    if ([IO.Path]::GetFullPath($Report.child_manifest.path) -cne [IO.Path]::GetFullPath($ManifestPath) -or
        $Report.child_manifest.sha256 -cne $ManifestSha256) {
        throw 'Native pre-loader report belongs to a different completed manifest.'
    }
    $a = $Report.attestation
    Assert-PreloaderFields $a @('schema_version','launcher_pid','child_pid','launcher_sha256','request_sha256','machine_plan_sha256','challenge','owned_images','mapped_image_paths') 'Native pre-loader attestation'
    Assert-PreloaderInteger $a.schema_version 1 'Attestation schema'
    Assert-PreloaderInteger $a.launcher_pid 4294967295 'Native launcher PID'
    Assert-PreloaderInteger $a.child_pid 4294967295 'Native child PID'
    if ($a.owned_images -isnot [System.Array] -or $a.mapped_image_paths -isnot [System.Array]) { throw 'Native object/mapping fields must be arrays.' }
    if ($a.schema_version -cne 1 -or $a.launcher_pid -le 0 -or $a.child_pid -le 0 -or
        $a.launcher_pid -eq $a.child_pid -or $a.launcher_sha256 -cne $MachinePlan.verifier_tools.preloader.sha256 -or
        $a.machine_plan_sha256 -cne $MachinePlanSha256 -or $a.request_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $a.challenge -cnotmatch '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$') {
        throw 'Native pre-loader peer/challenge/external authority mismatch.'
    }
    $images = @($a.owned_images)
    if ($images.Count -ne @($MachinePlan.verifier_tools.runtime_files).Count + 1 -or
        $images[0].source.sha256 -cne $RuntimeImageSha256) { throw 'Pre-loader omitted an approved native image.' }
    $paths = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $objects = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    for ($index = 0; $index -lt $images.Count; $index++) {
        $image = $images[$index]
        Assert-PreloaderFields $image @('source','staged_path','object') 'Pre-loader retained image'
        Assert-PreloaderFields $image.source @('path','sha256') 'Pre-loader source binding'
        Assert-PreloaderFields $image.object @('volume_serial','file_index','length') 'Pre-loader native object'
        Assert-PreloaderInteger $image.object.volume_serial 4294967295 'Native volume serial'
        Assert-PreloaderInteger $image.object.file_index ([uint64]::MaxValue) 'Native file index'
        Assert-PreloaderInteger $image.object.length 2147483648 'Native file size'
        if (-not [IO.Path]::IsPathFullyQualified($image.staged_path) -or
            -not $paths.Add($image.staged_path) -or $image.source.sha256 -cnotmatch '^[0-9a-f]{64}$' -or
            $image.object.volume_serial -lt 0 -or $image.object.file_index -le 0 -or
            $image.object.length -le 0 -or $image.object.length -gt 2147483648 -or
            -not $objects.Add("$($image.object.volume_serial):$($image.object.file_index)")) {
            throw 'Pre-loader object is duplicated, unbounded, or malformed.'
        }
        if ($index -gt 0) {
            $expected = $MachinePlan.verifier_tools.runtime_files[$index - 1]
            if ($image.source.path -cne $expected.path -or $image.source.sha256 -cne $expected.sha256) {
                throw 'Pre-loader runtime closure differs from the approved ordered closure.'
            }
            if ([IO.Path]::GetDirectoryName($image.staged_path) -cne [IO.Path]::GetDirectoryName($images[0].staged_path)) {
                throw 'Pre-loader images do not share one sealed namespace.'
            }
        }
    }
    $mapped = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($path in @($a.mapped_image_paths)) {
        if (-not $paths.Contains($path) -or -not $mapped.Add($path)) { throw 'Native mapped module is unowned or duplicated.' }
    }
    if (-not $mapped.Contains($images[0].staged_path)) { throw 'Native application mapping was not observed.' }
    foreach ($image in $images | Select-Object -Skip 1) {
        if ([IO.Path]::GetFileName($image.staged_path) -match '^(avcodec|avformat|avutil|avfilter|swscale|swresample)-' -and
            -not $mapped.Contains($image.staged_path)) { throw 'A required linked FFmpeg mapping was not observed.' }
    }
}

Export-ModuleMember -Function Assert-NativePreloaderReport
