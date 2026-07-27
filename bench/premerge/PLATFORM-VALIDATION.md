# Native platform validation

This runbook closes the native-filesystem validation gap for Windows and macOS. Run it from a native checkout on the platform being recorded. Cross-compilation, WSL, a Windows VM sharing a Linux-hosted directory, and a macOS build executed against a network-backed workspace do not substitute for the native runs below.

The tests are measurement-only. They do not change shipped filesystem behavior.

## Evidence rules

1. Record actual command output and actual filesystem details; do not record expected results as observations.
2. Run the negative controls before trusting the positive checks:
   - `atomic_pointer_observer_rejects_a_synchronized_two_phase_write` holds a pointer in a known truncated interval and proves the same concurrent observer used by the positive test rejects it before the write completes.
   - `native_sync_tree_records_nested_syncs_and_propagates_failures` records every test-build file/directory sync visit and injects a nested-file sync failure that must propagate.
   - `storage_probe_checkpoint_failure_is_observed_and_cleaned_up` injects a storage-probe checkpoint failure that must surface as `UnsuitableDataRoot` and leave no probe directory.
   - `native_publication_fault_instrument_proves_each_recovery_boundary` injects an error after each publication boundary and proves the fault is observed before recovery is assessed.
3. Run native tests with `--nocapture`.
4. The special network-environment tests are ignored by default. Explicit runs require `--ignored` and the named environment variable; missing setup fails instead of being reported as a passing test. If the environment is unavailable, record the branch as unvalidated rather than running a synthetic skip.
5. Do not claim power-loss durability from these tests. They prove that the native calls complete and that process-visible state obeys the tested invariants; they do not cut power between system calls.

## Test inventory and proof boundary

| Test | Native behavior exercised | What it proves | What it does not prove |
|---|---|---|---|
| `native_local_data_root_passes_platform_suitability_and_storage_probes` | Platform network-filesystem classifier; same-root exclusive-lock probe; synced staging tree; directory rename; two atomic pointer replacements; probe cleanup | A normal native temporary data root is accepted and the complete suitability probe succeeds on its actual filesystem | Power-loss persistence; other filesystem types or mount options |
| `storage_probe_checkpoint_failure_is_observed_and_cleaned_up` | Test-build fault checkpoint inside the real storage probe, after probe-directory creation | `prepare()` actually enters the probe, maps its failure to `UnsuitableDataRoot`, and removes the probe directory | Native filesystem failure behavior outside the injected checkpoint |
| `native_writer_lock_is_exclusive_across_processes` | `fs2` exclusive lock held by one test process and contended by a child test process | A second native process cannot acquire the data-root writer lock while the first owns it | Network-lock behavior; crash recovery of an OS with broken lock semantics |
| `atomic_pointer_observer_rejects_a_synchronized_two_phase_write` | The concurrent atomicity observer runs while a deliberately non-atomic writer pauses after syncing half a JSON value | The observer deterministically reads during writer activity and rejects the known torn interval before completion | Native rename or flush behavior; this is the negative control |
| `native_atomic_pointer_replacement_never_exposes_partial_state` | Repeated synced temporary-file writes and replacement of an existing pointer while a synchronized observer reads after every publication | Readers execute while the writer is active and observe only complete, internally consistent JSON values during all 64 replacements | Atomicity across sudden power loss; every possible antivirus/filter-driver interaction |
| `native_sync_tree_records_nested_syncs_and_propagates_failures` | Test-build instrumentation around nested file `sync_all`; directory `sync_all` on Unix/macOS; recorded directory no-op visits on Windows | Every expected nested file and platform-appropriate directory path is visited, and an injected nested-file sync failure propagates | That storage hardware honored cache flush after power loss; Windows directory durability beyond the implementation's current no-op directory sync |
| `native_publication_fault_instrument_proves_each_recovery_boundary` | Candidate sync, staging-to-generation rename, pointer replacement, and recovery after an injected interruption at each boundary | The fault hook fires at all three boundaries and recovery selects the documented old/new generation for each observed boundary | Kernel or hardware behavior under an actual crash |
| `missing_pointer_recovers_newest_complete_generation` | Native generation scan and atomic pointer reconstruction | A missing pointer selects the newest complete generation | Recovery from arbitrary media corruption |
| `corrupt_pointer_recovers_valid_predecessor` | Invalid pointer parsing, complete-generation validation, pointer reconstruction | A corrupt pointer recovers a valid complete predecessor and ignores an incomplete newer directory | Recovery when every retained generation is corrupt |
| `invalid_current_generation_recovers_a_valid_predecessor` | Index validation and predecessor selection | A pointer to an invalid generation is replaced with a valid predecessor | All Tantivy corruption modes |
| `network_filesystem_type_decision_logic_is_platform_independent` | Injected filesystem-type strings | The deny-list decision logic is portable and recognizes each listed network filesystem | Any live mount, Windows drive classification, or macOS `statfs` behavior |
| `mountinfo_decision_logic_uses_deepest_mount_and_decodes_paths` | Synthetic Linux mountinfo text | Deepest-mount selection and escaped-path decoding logic | Reading `/proc/self/mountinfo` or the host's real mount table |
| `windows_local_temp_directory_exercises_drive_type_classification` | Windows canonical path prefix handling and `GetDriveTypeW` on the native temp volume | The real local temp drive is not classified as `DRIVE_REMOTE` | UNC and mapped-network-drive branches |
| `windows_unc_prefix_is_rejected_on_a_real_share` | Real `UNC`/`VerbatimUNC` path created beneath `KWIRY_TEST_WINDOWS_UNC_ROOT` | The actual UNC prefix branch rejects the data root before the storage probe | Anything when no accessible UNC share is supplied |
| `windows_drive_remote_is_rejected_on_a_mapped_drive` | Real drive-letter path on a mapped network drive; canonical prefix assertion; independent `GetDriveTypeW == DRIVE_REMOTE` assertion; production rejection | Canonicalization remains `Disk`/`VerbatimDisk`, the OS reports `DRIVE_REMOTE`, and the mapped-drive branch rejects the data root rather than passing through UNC handling | Anything when no mapped remote drive is supplied |
| `macos_local_temp_directory_exercises_statfs_mnt_local` | Native `statfs` and `MNT_LOCAL` on the temp volume | The actual local-volume branch accepts the host's temp filesystem | Other local filesystem types or mount options |
| `macos_nonlocal_mount_is_rejected` | Native `statfs` on a directory beneath `KWIRY_TEST_MACOS_NETWORK_ROOT` | A real mount without `MNT_LOCAL` is rejected and its filesystem type is reported | Anything when no suitable network mount is supplied |

## Windows

Use a native PowerShell session. The checkout and `target` directory should be on a local Windows volume for the baseline run.

### Record the host

```powershell
Set-Location C:\path\to\kwir\daemon
git rev-parse HEAD
rustc -Vv
cargo -V
[System.Environment]::OSVersion.VersionString
Get-Volume | Format-Table DriveLetter, FileSystem, FileSystemLabel, DriveType, HealthStatus
```

### Mandatory local-volume suite

```powershell
cargo test -p kwiry-core --lib generation::tests::storage_probe_checkpoint_failure_is_observed_and_cleaned_up -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::native_local_data_root_passes_platform_suitability_and_storage_probes -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::native_writer_lock_is_exclusive_across_processes -- --exact --nocapture
cargo test -p kwiry-core --lib state::tests::atomic_pointer_observer_rejects_a_synchronized_two_phase_write -- --exact --nocapture
cargo test -p kwiry-core --lib state::tests::native_atomic_pointer_replacement_never_exposes_partial_state -- --exact --nocapture
cargo test -p kwiry-core --lib state::tests::native_sync_tree_records_nested_syncs_and_propagates_failures -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::native_publication_fault_instrument_proves_each_recovery_boundary -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::missing_pointer_recovers_newest_complete_generation -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::corrupt_pointer_recovers_valid_predecessor -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::invalid_current_generation_recovers_a_valid_predecessor -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::network_filesystem_type_decision_logic_is_platform_independent -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::mountinfo_decision_logic_uses_deepest_mount_and_decodes_paths -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::windows_local_temp_directory_exercises_drive_type_classification -- --exact --nocapture
```

The two decision-logic tests use injected data. On Windows they validate the shared pure logic only; they do not validate Linux mount discovery.

### UNC branch

Use a writable test folder on an actual UNC share. Do not use a local path written with extra slashes.

```powershell
$env:KWIRY_TEST_WINDOWS_UNC_ROOT = '\\server\share\kwiry-validation'
cargo test -p kwiry-core --lib generation::tests::windows_unc_prefix_is_rejected_on_a_real_share -- --exact --ignored --nocapture
Remove-Item Env:KWIRY_TEST_WINDOWS_UNC_ROOT
```

If no UNC environment is available, do not run this ignored test; record the UNC branch as unvalidated. An explicit run without the variable is intentionally a failure, not a skip or pass.

### `DRIVE_REMOTE` branch

Map a network share to a drive letter using the normal Windows mechanism, then point the test at a writable folder on that drive.

```powershell
$env:KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT = 'Z:\kwiry-validation'
cargo test -p kwiry-core --lib generation::tests::windows_drive_remote_is_rejected_on_a_mapped_drive -- --exact --ignored --nocapture
Remove-Item Env:KWIRY_TEST_WINDOWS_REMOTE_DRIVE_ROOT
```

If no mapped remote drive is available, do not run this ignored test; record the `DRIVE_REMOTE` branch as unvalidated. An explicit run without the variable intentionally fails. A UNC-only run does not cover `GetDriveTypeW == DRIVE_REMOTE`; both special-environment tests are required for complete Windows path coverage.

### Full Windows gate

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

The full workspace run leaves special network-environment tests ignored by design. Their ignored status is not native branch validation; run the explicit `--ignored` commands above with real mounts.

## macOS

Use a native Terminal session. Keep the checkout and `target` directory on a local volume for the baseline run.

### Record the host

```bash
cd /path/to/kwir/daemon
git rev-parse HEAD
rustc -Vv
cargo -V
sw_vers
mount
diskutil info / | grep -E 'File System Personality|Type \(Bundle\)|Volume Name|Device Location|Solid State'
```

### Mandatory local-volume suite

```bash
cargo test -p kwiry-core --lib generation::tests::storage_probe_checkpoint_failure_is_observed_and_cleaned_up -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::native_local_data_root_passes_platform_suitability_and_storage_probes -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::native_writer_lock_is_exclusive_across_processes -- --exact --nocapture
cargo test -p kwiry-core --lib state::tests::atomic_pointer_observer_rejects_a_synchronized_two_phase_write -- --exact --nocapture
cargo test -p kwiry-core --lib state::tests::native_atomic_pointer_replacement_never_exposes_partial_state -- --exact --nocapture
cargo test -p kwiry-core --lib state::tests::native_sync_tree_records_nested_syncs_and_propagates_failures -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::native_publication_fault_instrument_proves_each_recovery_boundary -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::missing_pointer_recovers_newest_complete_generation -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::corrupt_pointer_recovers_valid_predecessor -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::invalid_current_generation_recovers_a_valid_predecessor -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::network_filesystem_type_decision_logic_is_platform_independent -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::mountinfo_decision_logic_uses_deepest_mount_and_decodes_paths -- --exact --nocapture
cargo test -p kwiry-core --lib generation::tests::macos_local_temp_directory_exercises_statfs_mnt_local -- --exact --nocapture
```

The two decision-logic tests use injected data. On macOS they validate the shared pure logic only; they do not validate Linux mount discovery.

### Nonlocal `statfs` branch

Mount a writable SMB, NFS, or other network filesystem for which macOS reports no `MNT_LOCAL`, then set the variable to a directory on that mount.

```bash
export KWIRY_TEST_MACOS_NETWORK_ROOT='/Volumes/kwiry-validation'
cargo test -p kwiry-core --lib generation::tests::macos_nonlocal_mount_is_rejected -- --exact --ignored --nocapture
unset KWIRY_TEST_MACOS_NETWORK_ROOT
```

If no nonlocal mount is available, do not run this ignored test; record the nonlocal `statfs` branch as unvalidated. An explicit run without the variable intentionally fails. A cloud-synchronization folder on a local APFS volume is not sufficient: the test specifically needs `statfs` to omit `MNT_LOCAL`.

### Full macOS gate

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

The full workspace run leaves special network-environment tests ignored by design. Their ignored status is not native branch validation; run the explicit `--ignored` commands above with real mounts.

## Results template

Copy one section per machine and fill it with observed values. Preserve failing output verbatim and label unavailable special environments `UNVALIDATED`.

```text
Platform: Windows | macOS
Machine/runner:
Commit SHA:
Date/time:
OS version/build:
Architecture:
rustc -Vv:
cargo -V:
Checkout volume and filesystem:
Temporary-directory volume and filesystem:
Special network environment:
  UNC share available: yes/no/not applicable
  Mapped DRIVE_REMOTE available: yes/no/not applicable
  macOS nonlocal MNT_LOCAL-clear mount available: yes/no/not applicable

Negative controls
- synchronized torn-pointer observer: PASS/FAIL
  observed output:
- sync-path visit/failure instrumentation: PASS/FAIL
  observed output:
- storage-probe checkpoint/cleanup: PASS/FAIL
  observed output:
- publication fault boundaries: PASS/FAIL
  observed output:

Local native checks
- data-root suitability/storage probe: PASS/FAIL
- cross-process exclusive lock: PASS/FAIL
- repeated atomic pointer replacement: PASS/FAIL
- sync-tree flush path: PASS/FAIL
- missing-pointer recovery: PASS/FAIL
- corrupt-pointer predecessor recovery: PASS/FAIL
- invalid-current predecessor recovery: PASS/FAIL
- local Windows GetDriveTypeW or macOS MNT_LOCAL classifier: PASS/FAIL
- injected pure decision logic: PASS/FAIL

Special-environment checks
- Windows UNC prefix: PASS/FAIL/UNVALIDATED
  observed output or unavailable-environment reason:
- Windows mapped DRIVE_REMOTE: PASS/FAIL/UNVALIDATED
  observed output or unavailable-environment reason:
- macOS nonlocal statfs: PASS/FAIL/UNVALIDATED
  observed output or unavailable-environment reason:

Full gate
- cargo fmt --all --check: PASS/FAIL
- cargo clippy --workspace --all-targets --all-features -- -D warnings: PASS/FAIL
- cargo test --workspace: PASS/FAIL

Unexpected behavior, filesystem limitations, or unproved claims:
Raw log location or CI run URL:
Reviewer/date:
```
