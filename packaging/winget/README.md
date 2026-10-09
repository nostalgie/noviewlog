# Windows Package Manager (winget)

Source-of-truth manifests for [NoViewLog](https://github.com/nostalgie/noviewlog) on
the community [`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs) repo.

Layout mirrors winget-pkgs: copy `manifests/n/nostalgie/NoViewLog/<version>/` into
`manifests/n/nostalgie/NoViewLog/<version>/` on your fork.

**Package identifier:** `nostalgie.NoViewLog`

**Install (after the winget-pkgs PR is merged):**

```powershell
winget install nostalgie.NoViewLog
```

## Validate locally

From this directory (requires [App Installer / winget](https://learn.microsoft.com/en-us/windows/package-manager/winget/)):

```powershell
winget validate --manifest "manifests\n\nostalgie\NoViewLog\0.4.1"
```

## Submit a new package or version to winget-pkgs

1. Fork [`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs/fork).
2. Create a branch from `master`.
3. Add or update the three YAML files under
   `manifests/n/nostalgie/NoViewLog/<version>/` (use the copies in this folder).
4. Open a pull request against `microsoft/winget-pkgs` `master`.
   Follow [Contributing](https://github.com/microsoft/winget-pkgs/blob/master/CONTRIBUTING.md)
   and the [PR template](https://github.com/microsoft/winget-pkgs/blob/master/.github/PULL_REQUEST_TEMPLATE.md).

Alternatively, use [`wingetcreate`](https://github.com/microsoft/winget-create) to
generate manifests from a release URL, then align fields with the maintained copies here.

## Upgrade checklist (each public release)

When a new Windows zip is published on
[Releases](https://github.com/nostalgie/noviewlog/releases):

1. Note the tag (e.g. `v0.4.2`) and asset name `noviewlog-win-x64.zip`.
2. Compute SHA256 of the zip (must match the GitHub release asset):

   ```powershell
   Invoke-WebRequest -Uri "https://github.com/nostalgie/noviewlog/releases/download/vTAG/noviewlog-win-x64.zip" -OutFile noviewlog-win-x64.zip
   Get-FileHash -Algorithm SHA256 .\noviewlog-win-x64.zip
   ```

   Or read the `digest` field from `gh release view vTAG --repo nostalgie/noviewlog --json assets`.

3. Copy the previous version folder to the new version directory under
   `manifests/n/nostalgie/NoViewLog/`.
4. In all three YAML files, set `PackageVersion` to the new semver (no leading `v`).
5. In `nostalgie.NoViewLog.installer.yaml`, update `InstallerUrl`, `InstallerSha256`
   (uppercase hex), and `ReleaseDate`.
6. In `nostalgie.NoViewLog.locale.en-US.yaml`, update `ReleaseNotesUrl` if needed.
7. Run `winget validate` on the new folder; open a winget-pkgs PR.

For version upgrades (package already in the catalog), you may only need the new
version folder; winget-pkgs reviewers will confirm whether older version folders
should be removed per repo policy.
