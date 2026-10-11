<#
.SYNOPSIS
  ReGa machine agent. See README.md in infra/ansible.

.DESCRIPTION
  Restore     (boot, SYSTEM)  Copy the Steam library from S3 to the library disk.
  Tick        (every minute, SYSTEM)  Track idle time, upload finished
              downloads, stop the machine after idle_minutes with nobody streaming.
  FinalSync   (app, over WinRM)  Upload the library and record the outcome
              in S3, so the app knows whether it can delete the library disk.
  LaunchSteam (logon, desktop user)  Wait for the restore, register the
              library folder with Steam and start Steam.
  Status      Print the agent state as JSON.
  ResetIdle   Restart the idle clock (Ansible runs it after a setup).
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('Restore', 'Tick', 'FinalSync', 'LaunchSteam', 'Status', 'ResetIdle')]
  [string]$Action,

  [string]$ConfigPath
)

# Windows PowerShell 5.1 leaves $PSScriptRoot empty in parameter defaults.
if (-not $ConfigPath) { $ConfigPath = Join-Path $PSScriptRoot 'config.json' }

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'ReGa.Agent.psm1') -Force

$config = Read-JsonFile $ConfigPath
if ($null -eq $config) { throw "Missing agent config $ConfigPath" }

$stateDir = $config.state_dir
$statePath = Join-Path $stateDir 'agent.json'
$restorePath = Join-Path $stateDir 'restore.json'
$libraryPath = $config.library.path
$steamApps = Join-Path $libraryPath 'steamapps'
$marker = Get-RestoredMarkerPath $libraryPath

if ($Action -eq 'LaunchSteam') {
  # Runs as the desktop user, who can't write to the agent's folders.
  Set-ReGaLogPath (Join-Path $env:LOCALAPPDATA 'ReGa\steam-launch.log')
} else {
  Set-ReGaLogPath (Join-Path $config.log_dir 'agent.log')
}

function Get-BootTime {
  return (Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime()
}

function Get-Target {
  # The instance metadata service can be slow to answer right after boot.
  param([int]$Attempts = 1)
  $identity = $null
  for ($i = 1; $i -le $Attempts -and $null -eq $identity; $i++) {
    $identity = Get-Ec2Identity
    if ($null -eq $identity -and $i -lt $Attempts) { Start-Sleep -Seconds 10 }
  }
  return Resolve-LibraryTarget -Config $config.library -Identity $identity
}

function Get-FinalSyncKey {
  param($Target)
  return "s3://$($Target.Bucket)/rega/machines/$($config.machine_id)/final-sync.json"
}

function Remove-FinalSyncRecord {
  # Deleting a key that isn't there succeeds too.
  param($Target)
  $key = Get-FinalSyncKey $Target
  foreach ($wait in @(0, 5, 15)) {
    if ($wait -gt 0) { Start-Sleep -Seconds $wait }
    $code = Invoke-Aws -AwsPath $config.aws_path -Arguments @('s3', 'rm', $key, '--region', $Target.Region, '--only-show-errors')
    if ($code -eq 0) { return $true }
  }
  Write-ReGaLog "Could not delete $key (exit code $code)." 'error'
  return $false
}

function Initialize-LibraryFolder {
  # Steam needs steamapps\ and libraryfolder.vdf in each library, and the
  # desktop user must be able to write there.
  New-Item -ItemType Directory -Path $steamApps -Force | Out-Null
  $vdf = Join-Path $libraryPath 'libraryfolder.vdf'
  if (-not (Test-Path -LiteralPath $vdf)) {
    [IO.File]::WriteAllText($vdf, (New-LibraryFolderVdf (New-SteamContentId)), (New-Object Text.UTF8Encoding($false)))
  }
  & icacls.exe $libraryPath /grant '*S-1-5-32-545:(OI)(CI)M' /Q | Out-Null
}

function Invoke-Upload {
  param([switch]$All)
  return Invoke-LibraryUpload -AwsPath $config.aws_path -LibraryPath $libraryPath -MarkerPath $marker `
    -Target (Get-Target) -All:$All
}

function Invoke-FinalSync {
  # Upload, then record the outcome at a fixed key, successful or not. The
  # app deletes the library disk only for a record with synced = true that
  # was written after the machine's last start.
  $synced = $false
  $skipped = @()
  $failed = @()
  try {
    $upload = Invoke-Upload -All
    $synced = $upload.Ok
    $skipped = @($upload.Skipped)
    $failed = @($upload.Failed)
  } catch {
    Write-ReGaLog "Final upload failed: $_" 'error'
  }
  try {
    $target = Get-Target
    $identity = Get-Ec2Identity
    $record = [pscustomobject]@{
      machine_id   = $config.machine_id
      instance_id  = (Get-PropertyValue $identity 'instanceId' '')
      boot_utc     = (ConvertTo-IsoString (Get-BootTime))
      finished_utc = (ConvertTo-IsoString ([datetime]::UtcNow))
      synced       = $synced
      # Games this disk didn't upload because S3 has another machine's build.
      skipped_apps = $skipped
      # Games that didn't upload; the disk must be kept.
      failed_apps  = $failed
      library_uri  = $target.Uri
    }
    $file = Join-Path $stateDir 'final-sync.json'
    Write-JsonFile $file $record
    $key = Get-FinalSyncKey $target
    $code = Invoke-Aws -AwsPath $config.aws_path -Arguments @('s3', 'cp', $file, $key, '--region', $target.Region, '--only-show-errors')
    if ($code -ne 0) {
      Write-ReGaLog "Could not write $key (exit code $code)." 'error'
      return $false
    }
    # This record replaces any one the Tick still had to delete.
    Save-State @{ final_sync_stale = $false }
  } catch {
    Write-ReGaLog "Could not record the final sync: $_" 'error'
    return $false
  }
  return $synced
}

function Stop-Steam {
  param([string]$SteamExe)
  if (-not (Get-Process -Name 'steam' -ErrorAction SilentlyContinue)) { return }
  Write-ReGaLog 'Closing Steam.'
  & $SteamExe -shutdown
  # Generous, so a Steam that is updating itself can finish first.
  $wait = [datetime]::UtcNow.AddSeconds(300)
  while ((Get-Process -Name 'steam' -ErrorAction SilentlyContinue) -and [datetime]::UtcNow -lt $wait) { Start-Sleep -Seconds 2 }
  Get-Process -Name 'steam' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

function Save-State {
  param($Values)
  $state = Read-JsonFile $statePath
  $merged = [ordered]@{}
  if ($state) { foreach ($p in $state.PSObject.Properties) { $merged[$p.Name] = $p.Value } }
  foreach ($k in $Values.Keys) { $merged[$k] = $Values[$k] }
  Write-JsonFile $statePath ([pscustomobject]$merged)
}

function Get-Signals {
  $now = [datetime]::UtcNow
  $boot = Get-BootTime
  $restore = Read-JsonFile $restorePath
  $restoring = ((Get-PropertyValue $restore 'status') -eq 'running' -and
    (Test-SameBoot (Get-PropertyValue $restore 'boot_utc') $boot))
  $streaming = Test-SunshineStreaming $config.sunshine_log
  $downloading = Test-SteamDownloadActive -SteamAppsPaths @($config.steam_apps_paths) -Now $now
  $remote = Test-RemoteShellActive
  return [pscustomobject]@{
    Now         = $now
    Boot        = $boot
    Streaming   = $streaming
    Downloading = $downloading
    Restoring   = $restoring
    RemoteShell = $remote
    Active      = ($streaming -or $downloading -or $restoring -or $remote)
  }
}

switch ($Action) {
  'Restore' {
    $boot = Get-BootTime
    $status = [ordered]@{ status = 'running'; boot_utc = (ConvertTo-IsoString $boot); started_utc = (ConvertTo-IsoString ([datetime]::UtcNow)) }
    Write-JsonFile $restorePath ([pscustomobject]$status)
    $result = 'failed'
    try {
      # The boot script brings a new library disk online within a minute or
      # two; Terraform may attach it after Windows has started.
      $deadline = [datetime]::UtcNow.AddMinutes([int]$config.restore_wait_minutes)
      while (-not (Test-LibraryVolume $libraryPath) -and [datetime]::UtcNow -lt $deadline) { Start-Sleep -Seconds 10 }
      if (-not (Test-LibraryVolume $libraryPath)) {
        Write-ReGaLog "No ReGaLibrary volume for $libraryPath; nothing to restore." 'warning'
        $result = 'no-disk'
      } elseif (Test-Path -LiteralPath $marker) {
        Write-ReGaLog "$libraryPath was already restored this session."
        Initialize-LibraryFolder
        $result = 'done'
      } else {
        $target = Get-Target -Attempts 6
        # A record from an earlier session must not vouch for this disk. The
        # app also ignores one written before the machine's last start.
        Remove-FinalSyncRecord $target | Out-Null
        New-Item -ItemType Directory -Path $libraryPath -Force | Out-Null
        $arguments = Get-SyncArguments -Source $target.Uri -Destination $libraryPath -Region $target.Region `
          -Excludes $config.library.excludes
        $drive = [IO.Path]::GetPathRoot($libraryPath).TrimEnd('\').TrimEnd(':')
        $full = $false
        $code = 1
        foreach ($wait in @(0, 30, 60, 120, 240)) {
          if ($wait -gt 0) {
            Write-ReGaLog "Restore attempt failed with exit code $code; retrying in $wait seconds." 'warning'
            Start-Sleep -Seconds $wait
          }
          $code = Invoke-Aws -AwsPath $config.aws_path -Arguments $arguments
          if ($code -eq 0) { break }
          $volume = Get-Volume -DriveLetter $drive -ErrorAction SilentlyContinue
          if ($volume -and $volume.SizeRemaining -lt 1GB) { $full = $true; break }
        }
        if ($full) {
          # Retrying can't help: the library needs a bigger disk or fewer games.
          Write-ReGaLog "The library in $($target.Uri) doesn't fit on $libraryPath; uploads stay off this session." 'error'
          $result = 'disk-full'
        } elseif ($code -eq 0) {
          Initialize-LibraryFolder
          $snapshot = Get-LibrarySnapshot $steamApps
          Write-JsonFile $marker ([pscustomobject]@{
            restored_utc = (ConvertTo-IsoString ([datetime]::UtcNow))
            source       = $target.Uri
            apps         = $snapshot.apps
            stamps       = $snapshot.stamps
            builds       = $snapshot.builds
          })
          Write-ReGaLog "Restored $($target.Uri) to $libraryPath."
          Save-State @{ synced_fingerprint = (Get-LibraryFingerprint $steamApps) }
          $result = 'done'
        } else {
          Write-ReGaLog "Restore from $($target.Uri) failed with exit code $code; uploads stay off this session." 'error'
        }
      }
    } catch {
      Write-ReGaLog "Restore failed: $_" 'error'
    } finally {
      $status.status = $result
      $status['finished_utc'] = (ConvertTo-IsoString ([datetime]::UtcNow))
      Write-JsonFile $restorePath ([pscustomobject]$status)
    }
    if ($result -in @('failed', 'disk-full')) { exit 1 }
  }

  'Tick' {
    $signals = Get-Signals
    $state = Read-JsonFile $statePath
    $stopped = ConvertTo-UtcDate (Get-PropertyValue $state 'stopped_utc')
    if ($null -ne $stopped -and $stopped -gt $signals.Boot) {
      # Already shutting down since this boot.
      return
    }
    if (Get-PropertyValue $state 'final_sync_stale' $false) {
      try {
        if (Remove-FinalSyncRecord (Get-Target)) { Save-State @{ final_sync_stale = $false } }
      } catch {
        Write-ReGaLog "Could not delete the final-sync record: $_" 'error'
      }
    }
    $decision = Get-IdleDecision -State $state -Now $signals.Now -BootTime $signals.Boot -Active $signals.Active `
      -IdleMinutes ([int]$config.idle_minutes) -IdleStopEnabled ([bool]$config.idle_stop_enabled)
    Save-State @{ last_active_utc = (ConvertTo-IsoString $decision.LastActiveUtc); idle_minutes = $decision.IdleMinutes }

    # Upload finished downloads and updates, but not while someone is
    # playing: the upload competes with the game for disk throughput.
    if (-not $decision.ShouldStop -and -not $signals.Streaming -and -not $signals.Downloading -and
        -not $signals.Restoring -and (Test-Path -LiteralPath $marker)) {
      $fingerprint = Get-LibraryFingerprint $steamApps
      if ($fingerprint -ne (Get-PropertyValue $state 'synced_fingerprint' '')) {
        try {
          if ((Invoke-Upload).Ok) { Save-State @{ synced_fingerprint = $fingerprint; last_sync_utc = (ConvertTo-IsoString ([datetime]::UtcNow)) } }
        } catch {
          Write-ReGaLog "Upload failed: $_" 'error'
        }
      }
    }

    if ($decision.ShouldStop) {
      if ($null -eq (Get-Ec2Identity)) {
        Write-ReGaLog 'Idle, but this is not an EC2 instance; not shutting down.' 'warning'
        return
      }
      Write-ReGaLog "No stream, download or remote session for $($decision.IdleMinutes) minutes; syncing and stopping."
      $synced = Invoke-FinalSync
      if ((Get-Signals).Active) {
        # Someone connected during the upload, which can take minutes.
        Write-ReGaLog 'Activity during the final upload; not stopping.'
        # The record would vouch for a disk that keeps changing, for example
        # if the CloudWatch alarm stops the machine later. Until it is gone,
        # every tick tries again.
        $removed = $false
        try {
          $removed = Remove-FinalSyncRecord (Get-Target)
        } catch {
          Write-ReGaLog "Could not delete the final-sync record: $_" 'error'
        }
        Save-State @{ last_active_utc = (ConvertTo-IsoString ([datetime]::UtcNow)); final_sync_stale = (-not $removed) }
        return
      }
      if (-not $synced) { Write-ReGaLog 'Stopping without a final upload; the app keeps the library disk.' 'warning' }
      Save-State @{ stopped_utc = (ConvertTo-IsoString ([datetime]::UtcNow)); final_sync = $synced }
      # An OS shutdown stops an on-demand instance (and ends a spot one).
      & shutdown.exe /s /t 30 /d p:0:0 /c 'ReGa: idle, stopping the machine.'
    }
  }

  'FinalSync' {
    $ok = Invoke-FinalSync
    [pscustomobject]@{ synced = $ok } | ConvertTo-Json -Compress
    if (-not $ok) { exit 1 }
  }

  'LaunchSteam' {
    $steamExe = Join-Path $config.steam_path 'steam.exe'
    if (-not (Test-Path -LiteralPath $steamExe)) {
      Write-ReGaLog "Steam is not installed at $steamExe." 'error'
      exit 1
    }
    # Steam puts itself in the user's Run key and would start at logon on a
    # library that is still being restored. Take it out, and close a Steam
    # that already started this way.
    Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Steam' -ErrorAction SilentlyContinue
    Stop-Steam $steamExe

    # Wait until this boot's restore has finished, so Steam sees the
    # restored games as installed.
    # Give the boot task a few minutes to start, then wait while it runs.
    $boot = Get-BootTime
    $startBy = [datetime]::UtcNow.AddMinutes(5)
    $deadline = [datetime]::UtcNow.AddMinutes(180)
    while ([datetime]::UtcNow -lt $deadline) {
      # Explorer may still start Steam from the Run key after the check above.
      Stop-Steam $steamExe
      $restore = Read-JsonFile $restorePath
      $sameBoot = Test-SameBoot (Get-PropertyValue $restore 'boot_utc') $boot
      if ($sameBoot -and (Get-PropertyValue $restore 'status') -ne 'running') { break }
      if (-not $sameBoot -and [datetime]::UtcNow -gt $startBy) { break }
      Start-Sleep -Seconds 10
    }
    # Steam rewrites libraryfolders.vdf on exit, so close it before editing.
    Stop-Steam $steamExe
    if (Test-Path -LiteralPath (Join-Path $libraryPath 'libraryfolder.vdf')) {
      try {
        $folders = Join-Path $config.steam_path 'steamapps\libraryfolders.vdf'
        $text = ''
        if (Test-Path -LiteralPath $folders) { $text = [IO.File]::ReadAllText($folders) }
        $contentId = Get-VdfContentId ([IO.File]::ReadAllText((Join-Path $libraryPath 'libraryfolder.vdf')))
        if (-not $contentId) { $contentId = New-SteamContentId }
        $updated = Add-SteamLibraryFolder -Text $text -SteamPath $config.steam_path -LibraryPath $libraryPath -ContentId $contentId
        if ($null -ne $updated) {
          New-Item -ItemType Directory -Path (Split-Path -Parent $folders) -Force | Out-Null
          [IO.File]::WriteAllText($folders, $updated, (New-Object Text.UTF8Encoding($false)))
          Write-ReGaLog "Added $libraryPath to Steam's library folders."
        }
      } catch {
        Write-ReGaLog "Could not register $libraryPath with Steam: $_" 'error'
      }
    }
    Start-Process -FilePath $steamExe
  }

  'Status' {
    $signals = Get-Signals
    [pscustomobject]@{
      machine_id   = $config.machine_id
      streaming    = $signals.Streaming
      downloading  = $signals.Downloading
      restoring    = $signals.Restoring
      remote_shell = $signals.RemoteShell
      restored     = (Test-Path -LiteralPath $marker)
      restore      = (Read-JsonFile $restorePath)
      state        = (Read-JsonFile $statePath)
    } | ConvertTo-Json -Depth 4
  }

  'ResetIdle' {
    Save-State @{ last_active_utc = (ConvertTo-IsoString ([datetime]::UtcNow)) }
  }
}
