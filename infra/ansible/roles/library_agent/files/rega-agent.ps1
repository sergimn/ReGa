<#
.SYNOPSIS
  ReGa machine agent. See README.md in infra/ansible.

.DESCRIPTION
  Restore     (boot, SYSTEM)  Copy the Steam library from S3 to the library disk.
  Tick        (every minute, SYSTEM)  Track idle time, upload finished
              downloads, stop the machine after idle_minutes with nobody streaming.
  FinalSync   (app, over WinRM)  Upload the library and record it in S3 so
              the app can delete the library disk.
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
  $identity = Get-Ec2Identity
  return Resolve-LibraryTarget -Config $config.library -Identity $identity
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

function Invoke-Sync {
  # Upload the library. --delete mirrors uninstalls and replaced files; it is
  # only safe after this disk was restored from S3, which the marker proves.
  param([switch]$Final)
  if (-not (Test-Path -LiteralPath $marker)) {
    Write-ReGaLog "Skipping upload: $libraryPath was not restored from S3 this session." 'warning'
    return $false
  }
  $target = Get-Target
  $arguments = Get-SyncArguments -Source $libraryPath -Destination $target.Uri -Region $target.Region `
    -Excludes $config.library.excludes -Delete
  $code = Invoke-Aws -AwsPath $config.aws_path -Arguments $arguments
  if ($code -ne 0) {
    Write-ReGaLog "Upload to $($target.Uri) failed with exit code $code." 'error'
    return $false
  }
  if ($Final) {
    # The app deletes the library disk only after it sees this record.
    $identity = Get-Ec2Identity
    $record = [pscustomobject]@{
      machine_id    = $config.machine_id
      instance_id   = (Get-PropertyValue $identity 'instanceId' '')
      finished_utc  = (ConvertTo-IsoString ([datetime]::UtcNow))
      library_uri   = $target.Uri
    }
    $tmp = Join-Path $stateDir 'final-sync.json'
    Write-JsonFile $tmp $record
    $key = "s3://$($target.Bucket)/rega/machines/$($config.machine_id)/final-sync.json"
    $code = Invoke-Aws -AwsPath $config.aws_path -Arguments @('s3', 'cp', $tmp, $key, '--region', $target.Region, '--only-show-errors')
    if ($code -ne 0) {
      Write-ReGaLog "Could not write $key (exit code $code)." 'error'
      return $false
    }
  }
  Write-ReGaLog "Uploaded $libraryPath to $($target.Uri)."
  return $true
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
  $sunshineLog = $config.sunshine_log
  $clients = Get-SunshineClientCount (Read-SharedTextFile $sunshineLog)
  $downloading = Test-SteamDownloadActive -SteamAppsPaths @($config.steam_apps_paths) -Now $now
  $remote = Test-RemoteShellActive
  return [pscustomobject]@{
    Now         = $now
    Boot        = $boot
    Clients     = $clients
    Downloading = $downloading
    Restoring   = $restoring
    RemoteShell = $remote
    Active      = ($clients -gt 0 -or $downloading -or $restoring -or $remote)
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
        $target = Get-Target
        New-Item -ItemType Directory -Path $libraryPath -Force | Out-Null
        $arguments = Get-SyncArguments -Source $target.Uri -Destination $libraryPath -Region $target.Region `
          -Excludes $config.library.excludes
        $code = Invoke-Aws -AwsPath $config.aws_path -Arguments $arguments
        if ($code -eq 0) {
          Initialize-LibraryFolder
          Write-JsonFile $marker ([pscustomobject]@{ restored_utc = (ConvertTo-IsoString ([datetime]::UtcNow)); source = $target.Uri })
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
    if ($result -eq 'failed') { exit 1 }
  }

  'Tick' {
    $signals = Get-Signals
    $state = Read-JsonFile $statePath
    $stopped = ConvertTo-UtcDate (Get-PropertyValue $state 'stopped_utc')
    if ($null -ne $stopped -and $stopped -gt $signals.Boot) {
      # Already shutting down since this boot.
      return
    }
    $decision = Get-IdleDecision -State $state -Now $signals.Now -BootTime $signals.Boot -Active $signals.Active `
      -IdleMinutes ([int]$config.idle_minutes) -IdleStopEnabled ([bool]$config.idle_stop_enabled)
    Save-State @{ last_active_utc = (ConvertTo-IsoString $decision.LastActiveUtc); idle_minutes = $decision.IdleMinutes }

    # Upload finished downloads and updates, but not while someone is
    # playing: the upload competes with the game for disk throughput.
    if (-not $decision.ShouldStop -and $signals.Clients -eq 0 -and -not $signals.Downloading -and
        -not $signals.Restoring -and (Test-Path -LiteralPath $marker)) {
      $fingerprint = Get-LibraryFingerprint $steamApps
      if ($fingerprint -ne (Get-PropertyValue $state 'synced_fingerprint' '')) {
        try {
          if (Invoke-Sync) { Save-State @{ synced_fingerprint = $fingerprint; last_sync_utc = (ConvertTo-IsoString ([datetime]::UtcNow)) } }
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
      $synced = $false
      try { $synced = Invoke-Sync -Final } catch { Write-ReGaLog "Final upload failed: $_" 'error' }
      if (-not $synced) { Write-ReGaLog 'Stopping without a final upload; the app keeps the library disk.' 'warning' }
      Save-State @{ stopped_utc = (ConvertTo-IsoString ([datetime]::UtcNow)); final_sync = $synced }
      # An OS shutdown stops an on-demand instance (and ends a spot one).
      & shutdown.exe /s /t 30 /d p:0:0 /c 'ReGa: idle, stopping the machine.'
    }
  }

  'FinalSync' {
    $ok = $false
    try { $ok = Invoke-Sync -Final } catch { Write-ReGaLog "Final upload failed: $_" 'error' }
    [pscustomobject]@{ synced = $ok } | ConvertTo-Json -Compress
    if (-not $ok) { exit 1 }
  }

  'LaunchSteam' {
    # Wait until this boot's restore has finished, so Steam sees the
    # restored games as installed.
    # Give the boot task a few minutes to start, then wait while it runs.
    $boot = Get-BootTime
    $startBy = [datetime]::UtcNow.AddMinutes(5)
    $deadline = [datetime]::UtcNow.AddMinutes(180)
    while ([datetime]::UtcNow -lt $deadline) {
      $restore = Read-JsonFile $restorePath
      $sameBoot = Test-SameBoot (Get-PropertyValue $restore 'boot_utc') $boot
      if ($sameBoot -and (Get-PropertyValue $restore 'status') -ne 'running') { break }
      if (-not $sameBoot -and [datetime]::UtcNow -gt $startBy) { break }
      Start-Sleep -Seconds 10
    }
    $steamExe = Join-Path $config.steam_path 'steam.exe'
    if (-not (Test-Path -LiteralPath $steamExe)) {
      Write-ReGaLog "Steam is not installed at $steamExe." 'error'
      exit 1
    }
    # Steam may have started on its own at logon and would overwrite
    # libraryfolders.vdf on exit, so close it before editing.
    if (Get-Process -Name 'steam' -ErrorAction SilentlyContinue) {
      & $steamExe -shutdown
      $wait = [datetime]::UtcNow.AddSeconds(90)
      while ((Get-Process -Name 'steam' -ErrorAction SilentlyContinue) -and [datetime]::UtcNow -lt $wait) { Start-Sleep -Seconds 2 }
    }
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
      clients      = $signals.Clients
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
