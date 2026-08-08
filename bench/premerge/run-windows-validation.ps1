# SPDX-FileCopyrightText: 2026 cybersader
# SPDX-License-Identifier: GPL-3.0-only
#
# Runs the native Windows validation suite and writes a report you can hand back.
#
# Usage, from a native Windows PowerShell session in the repo root:
#   .\bench\premerge\run-windows-validation.ps1
#   .\bench\premerge\run-windows-validation.ps1 -UncRoot '\\server\share\kwiry-validation'
#   .\bench\premerge\run-windows-validation.ps1 -RemoteDriveRoot 'Z:\kwiry-validation'
#
# The optional roots exercise the two network-rejection branches. Omitting one
# records that branch as UNVALIDATED rather than passing it by default: a branch
# nobody ran must never read as a branch that worked.

[CmdletBinding()]
param(
  [string]$UncRoot,
  [string]$RemoteDriveRoot,
  [string]$ReportPath = "bench\premerge\evidence\windows-validation.txt"
)

$ErrorActionPreference = 'Continue'

# Locate the repo from this script's own location rather than the caller's
# working directory, so it runs correctly from anywhere.
$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not (Test-Path (Join-Path $repoRoot 'daemon\Cargo.toml'))) {
  Write-Host "Could not find daemon\Cargo.toml above this script ($PSScriptRoot)." -ForegroundColor Red
  Write-Host "Expected the script to live at <repo>\bench\premerge\." -ForegroundColor Red
  exit 1
}
# A checkout reached over \\wsl$\ or a mapped drive is a network path. The whole
# point of this suite is native local-volume behavior, so refuse rather than
# produce evidence that looks native and is not.
if ($repoRoot -like '\\*') {
  Write-Host "This checkout is on a network path ($repoRoot)." -ForegroundColor Red
  Write-Host "Clone to a local volume (e.g. C:\src\kwir) and re-run. WSL paths do not substitute." -ForegroundColor Red
  exit 1
}

# Resolve the report against the repo, not the working directory, because the
# cargo invocations below run from daemon\.
if (-not [System.IO.Path]::IsPathRooted($ReportPath)) {
  $ReportPath = Join-Path $repoRoot $ReportPath
}
$reportDir = Split-Path -Parent $ReportPath
if ($reportDir -and -not (Test-Path $reportDir)) {
  New-Item -ItemType Directory -Path $reportDir -Force | Out-Null
}

# The Cargo workspace root is daemon\, not the repository root. Every cargo
# invocation below must run from there or it cannot find the manifest.
Set-Location (Join-Path $repoRoot 'daemon')

$lines = New-Object System.Collections.Generic.List[string]
function Emit([string]$text) {
  Write-Host $text
  $lines.Add($text)
}

Emit "Kwiry native Windows validation"
Emit "generated: $(Get-Date -Format 'yyyy-MM-ddTHH:mm:sszzz')"
Emit "repo root: $repoRoot"
Emit ""
Emit "== host =="
$commit = (git -C $repoRoot rev-parse HEAD 2>$null)
if (-not $commit) { $commit = 'unavailable (source copy without .git)' }
Emit "commit:   $commit"
Emit "os:       $([System.Environment]::OSVersion.VersionString)"
Emit "rustc:    $((rustc -V 2>&1) -join ' ')"
Emit "cargo:    $((cargo -V 2>&1) -join ' ')"
try {
  $drive = (Get-Item $repoRoot).PSDrive.Name
  $vol = Get-Volume -DriveLetter $drive -ErrorAction Stop
  Emit "volume:   $($drive): $($vol.FileSystem) $($vol.DriveType)"
} catch {
  Emit "volume:   unavailable ($($_.Exception.Message))"
}
Emit ""

$mandatory = @(
  'generation::tests::storage_probe_checkpoint_failure_is_observed_and_cleaned_up',
  'generation::tests::native_local_data_root_passes_platform_suitability_and_storage_probes',
  'generation::tests::native_writer_lock_is_exclusive_across_processes',
  'state::tests::atomic_pointer_observer_rejects_a_synchronized_two_phase_write',
  'state::tests::native_atomic_pointer_replacement_never_exposes_partial_state',
  'state::tests::native_sync_tree_records_nested_syncs_and_propagates_failures',
  'generation::tests::native_publication_fault_instrument_proves_each_recovery_boundary',
  'generation::tests::missing_pointer_recovers_newest_complete_generation',
  'generation::tests::corrupt_pointer_recovers_valid_predecessor',
  'generation::tests::invalid_current_generation_recovers_a_valid_predecessor',
  'generation::tests::network_filesystem_type_decision_logic_is_platform_independent',
  'generation::tests::mountinfo_decision_logic_uses_deepest_mount_and_decodes_paths',
  'generation::tests::windows_local_temp_directory_exercises_drive_type_classification'
)

$results = @{}

function Invoke-KwiryTest {
  param([string]$Name, [switch]$Ignored)
  # Not $args: that is an automatic PowerShell variable inside a function.
  $cargoArgs = @('test','-p','kwiry-core','--lib',$Name,'--','--exact','--nocapture')
  if ($Ignored) { $cargoArgs += '--ignored' }
  # Merge stderr so a panic message lands in the report, not just an exit code.
  $output = & cargo @cargoArgs 2>&1
  $ok = ($LASTEXITCODE -eq 0)
  $status = if ($ok) { 'PASS' } else { 'FAIL' }
  Emit ("  {0,-6} {1}" -f $status, $Name)
  if (-not $ok) {
    foreach ($line in ($output | Select-Object -Last 25)) { $lines.Add("         $line") }
  }
  return $ok
}

Emit "== mandatory local-volume suite =="
$failed = 0
foreach ($name in $mandatory) {
  if (-not (Invoke-KwiryTest -Name $name)) { $failed++ }
}
$results['mandatory'] = if ($failed -eq 0) { "PASS ($($mandatory.Count)/$($mandatory.Count))" }
                        else { "FAIL ($failed of $($mandatory.Count) failed)" }
Emit ""

Emit "== UNC rejection branch =="
if ($UncRoot) {
  if ($UncRoot -notlike '\\*') {
    Emit "  SKIP   -UncRoot must be a real UNC path (\\server\share\...), got: $UncRoot"
    $results['unc'] = 'UNVALIDATED (bad argument)'
  } else {
    $env:KWIRY_TEST_WINDOWS_UNC_ROOT = $UncRoot
    $ok = Invoke-KwiryTest -Name 'generation::tests::windows_unc_prefix_is_rejected_on_a_real_share' -Ignored
    Remove-Item Env:KWIRY_TEST_WINDOWS_UNC_ROOT -ErrorAction SilentlyContinue
    $results['unc'] = if ($ok) { "PASS (against $UncRoot)" } else { 'FAIL' }
  }
} else {
  Emit "  UNVALIDATED  no -UncRoot supplied; branch not exercised"
  $results['unc'] = 'UNVALIDATED (not run)'
}
Emit ""

Emit "== DRIVE_REMOTE rejection branch =="
if ($RemoteDriveRoot) {
  if ($RemoteDriveRoot -notmatch '^[A-Za-z]:\\') {
    Emit "  SKIP   -RemoteDriveRoot must be a drive-letter path (Z:\...), got: $RemoteDriveRoot"
    $results['remote'] = 'UNVALIDATED (bad argument)'
  } else {
    $env:KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT = $RemoteDriveRoot
    $ok = Invoke-KwiryTest -Name 'generation::tests::windows_drive_remote_is_rejected_on_a_mapped_drive' -Ignored
    Remove-Item Env:KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT -ErrorAction SilentlyContinue
    $results['remote'] = if ($ok) { "PASS (against $RemoteDriveRoot)" } else { 'FAIL' }
  }
} else {
  Emit "  UNVALIDATED  no -RemoteDriveRoot supplied; branch not exercised"
  $results['remote'] = 'UNVALIDATED (not run)'
}
Emit ""

Emit "== full workspace gate =="
& cargo fmt --all --check 2>&1 | Out-Null
$fmtOk = ($LASTEXITCODE -eq 0)
Emit ("  {0,-6} cargo fmt --all --check" -f $(if ($fmtOk) { 'PASS' } else { 'FAIL' }))

$clippy = & cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1
$clippyOk = ($LASTEXITCODE -eq 0)
Emit ("  {0,-6} cargo clippy -D warnings" -f $(if ($clippyOk) { 'PASS' } else { 'FAIL' }))
if (-not $clippyOk) { foreach ($l in ($clippy | Select-Object -Last 30)) { $lines.Add("         $l") } }

$testOut = & cargo test --workspace 2>&1
$testOk = ($LASTEXITCODE -eq 0)
Emit ("  {0,-6} cargo test --workspace" -f $(if ($testOk) { 'PASS' } else { 'FAIL' }))
foreach ($l in ($testOut | Select-String -Pattern '^test result:')) { $lines.Add("         $l") ; Write-Host "         $l" }
if (-not $testOk) { foreach ($l in ($testOut | Select-String -Pattern '^(error|failures:|---- )' | Select-Object -First 30)) { $lines.Add("         $l") } }
Emit ""

Emit "== summary =="
Emit "  mandatory local suite : $($results['mandatory'])"
Emit "  UNC branch            : $($results['unc'])"
Emit "  DRIVE_REMOTE branch   : $($results['remote'])"
Emit "  fmt / clippy / tests  : $(if ($fmtOk -and $clippyOk -and $testOk) { 'PASS' } else { 'FAIL' })"
Emit ""
Emit "macOS is not covered by this run and remains unvalidated."

$lines | Set-Content -Path $ReportPath -Encoding UTF8
Write-Host ""
Write-Host "Report written to $ReportPath" -ForegroundColor Green

$allOk = ($failed -eq 0) -and $fmtOk -and $clippyOk -and $testOk
exit $(if ($allOk) { 0 } else { 1 })
