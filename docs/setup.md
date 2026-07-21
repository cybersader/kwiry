# Setup and per-user service

`kwiry setup` is the guided onboarding path for a native Windows or Linux desktop. It registers one Markdown/text tree, records the semantic-search preference, prepares the disposable index, installs the daemon as a per-user service, starts it, and waits for authenticated readiness.

Native installers are not published yet. Until release packaging lands, build the binary from source and invoke the resulting `kwiry` or `kwiry.exe` executable.

## Supported environments

| Environment | Lifecycle manager | Behavior |
|---|---|---|
| Windows 11 | Current-user Task Scheduler task | Starts at user logon with least privilege and no stored password |
| Native Ubuntu/Linux | `systemd --user` | Starts with the user service manager; no `sudo` and no lingering |
| WSL | None | `setup` and mutating `service` commands are rejected; manual `index`, `search`, and `serve` remain available for development |

Setup never installs a system service, requests administrator access, enables Linux lingering, or copies the bearer token into a service definition.

## Guided setup

Run:

```text
kwiry setup
```

The wizard asks for only:

1. the Markdown/text tree, defaulting to the current directory;
2. a stable lowercase ID suggested from the directory name;
3. whether to enable semantic and hybrid search;
4. final confirmation of the ordered action plan.

Fresh setup defaults semantic search to **yes** after displaying the measured first-run costs: approximately **133 MB** downloaded and up to approximately **784 MiB** peak memory while indexing. A rerun preserves the saved preference unless `--semantic` or `--no-semantic` is supplied.

You can supply the values directly:

```text
kwiry setup /absolute/path/to/notes --id notes --no-semantic --yes
```

The default readiness deadline is 60 seconds for lexical-only setup and 900 seconds when semantic search is enabled. Override it with `--timeout SECONDS`.

## Dry-run and JSON automation

Dry-run inspects existing state and produces the same setup plan without creating config, token, index, service, descriptor, or process state:

```text
kwiry setup /absolute/path/to/notes --id notes --no-semantic --dry-run
```

For automation, add `--json`. JSON mode never prompts and writes exactly one versioned document to stdout:

```text
kwiry setup /absolute/path/to/notes --id notes --no-semantic --dry-run --json
```

A non-interactive mutating run requires `--yes`. Missing required values return a nonzero exit status and a stable error code such as `prompt_required`. Reports contain paths and service state but never the bearer-token value or authorization header.

## Service lifecycle

The service commands operate only on Kwiry's generated per-user lifecycle definition:

```text
kwiry service install
kwiry service start
kwiry service stop
kwiry service restart
kwiry service status
kwiry service uninstall
```

Mutating commands support `--dry-run` and `--json`; status supports `--json`.

`service uninstall` removes only the generated systemd unit or Task Scheduler task. It preserves registrations, configuration, tokens, downloaded models, indexes, logs, and the connection descriptor.

### Windows behavior

Kwiry creates a current-user Task Scheduler task named `Kwiry` with:

- an at-logon trigger;
- `InteractiveToken` and `LeastPrivilege`;
- no stored password and no elevation;
- an absolute executable path plus explicit config/data arguments;
- one running instance at a time;
- start-when-available and bounded restart-on-failure;
- permission to start and continue on battery power.

All scheduler operations invoke `schtasks.exe` directly—never PowerShell, `cmd.exe`, or a batch wrapper.

### Linux behavior

Kwiry writes an owner-controlled `kwiry.service` under the standard user systemd configuration directory. The unit uses `Restart=on-failure`, `UMask=0077`, and `NoNewPrivileges=true`.

The service runs with the user session. Kwiry does not call `loginctl enable-linger`, so it does not promise boot-before-login operation.

## Readiness and recovery

Setup reports success only after all of the following are true:

- the loopback-only connection descriptor is valid;
- public `/v0/health` responds successfully;
- the token can be loaded locally and authenticated `/v0/status` succeeds;
- the daemon is ready with a published generation and clean state;
- the requested tree is present, clean, and has no last error;
- the semantic model is loaded when semantic search was requested.

If a later service or readiness stage fails, earlier valid config and index work is retained. Fix the reported issue and rerun the same `kwiry setup` command; setup recomputes the plan and resumes safely.

The final summary shows the loopback URL when ready, the non-secret connection-descriptor path, and the logs directory. It never prints the token value.

## Manual development flow

The original commands remain available for development and unsupported lifecycle environments:

```text
kwiry vault add --id notes --path /absolute/path/to/notes
kwiry index
kwiry search "your query"
kwiry serve
```

Add `serve --semantic` to override the saved preference for that manual daemon run.
