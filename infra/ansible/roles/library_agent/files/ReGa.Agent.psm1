# ReGa machine agent: restores the Steam library from S3 at boot, uploads
# finished downloads and updates, and stops the machine when nobody has been
# streaming for idle_minutes. Runs under Windows PowerShell 5.1 as SYSTEM
# (scheduled tasks) and as the desktop user (Steam launch at logon).
#
# Functions that only compute are kept free of side effects so the Pester
# tests in infra/ansible/tests/pester can run them on any OS.

Set-StrictMode -Version 3.0

$script:LogPath = $null

function ConvertTo-UtcDate {
  # PowerShell 7 turns ISO 8601 strings into DateTime while parsing JSON;
  # Windows PowerShell 5.1 leaves them as strings. Accept both.
  param($Value)
  if ($null -eq $Value -or "$Value" -eq '') { return $null }
  if ($Value -is [datetime]) { return $Value.ToUniversalTime() }
  return [datetime]::Parse("$Value", [Globalization.CultureInfo]::InvariantCulture,
    [Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime()
}

function ConvertTo-IsoString {
  param([datetime]$Value)
  return $Value.ToUniversalTime().ToString('o', [Globalization.CultureInfo]::InvariantCulture)
}

function Set-ReGaLogPath {
  param([string]$Path)
  $script:LogPath = $Path
}

function Write-ReGaLog {
  param([string]$Message, [string]$Level = 'info')
  $line = '{0} [{1}] {2}' -f (ConvertTo-IsoString ([datetime]::UtcNow)), $Level, $Message
  if (-not $script:LogPath) {
    Write-Verbose $line
    return
  }
  try {
    $dir = Split-Path -Parent $script:LogPath
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $item = Get-Item -LiteralPath $script:LogPath -ErrorAction SilentlyContinue
    if ($item -and $item.Length -gt 5MB) {
      Move-Item -LiteralPath $script:LogPath -Destination "$($script:LogPath).1" -Force
    }
    Add-Content -LiteralPath $script:LogPath -Value $line -Encoding UTF8
  } catch {
    Write-Verbose "Could not write the agent log: $_"
  }
}

function Read-JsonFile {
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) { return $null }
  try {
    return Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
  } catch {
    Write-ReGaLog "Ignoring unreadable JSON in ${Path}: $_" 'warning'
    return $null
  }
}

function Write-JsonFile {
  # Write to a temporary file and move it into place, so a reader never sees
  # half a file.
  param([string]$Path, $Value)
  $dir = Split-Path -Parent $Path
  if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
  # Several processes write the agent state; give each its own temporary
  # file.
  $tmp = '{0}.{1}.tmp' -f $Path, [guid]::NewGuid().ToString('N')
  $json = $Value | ConvertTo-Json -Depth 6
  [IO.File]::WriteAllText($tmp, $json, (New-Object Text.UTF8Encoding($false)))
  Move-Item -LiteralPath $tmp -Destination $Path -Force
}

function Get-PropertyValue {
  # Read a property that may be missing, under StrictMode.
  param($Object, [string]$Name, $Default = $null)
  if ($null -eq $Object) { return $Default }
  $prop = $Object.PSObject.Properties[$Name]
  if ($null -eq $prop -or $null -eq $prop.Value) { return $Default }
  return $prop.Value
}

# --- Streaming sessions -------------------------------------------------------

function Get-SunshineClientCount {
  # Fallback for Test-SunshineStreaming. Sunshine rotates its log at every
  # start and writes "CLIENT CONNECTED" and "CLIENT DISCONNECTED" when a
  # Moonlight control stream opens or closes, and "<address>: Ping Timeout"
  # when it drops a client that stopped answering. Count them after the last
  # startup line, so a crash can't leave a session open forever.
  param([string[]]$Lines)
  $connected = 0
  foreach ($line in $Lines) {
    if ($line -match ': Sunshine version: ') {
      $connected = 0
    } elseif ($line -match 'CLIENT CONNECTED\s*$') {
      $connected++
    } elseif ($line -match 'CLIENT DISCONNECTED\s*$' -or $line -match '\S: Ping Timeout\s*$') {
      $connected = [Math]::Max(0, $connected - 1)
    }
  }
  return $connected
}

function Test-SunshineStreaming {
  # Sunshine raises its own priority class to High when the first stream
  # starts and puts it back to Normal when the last one ends, however it
  # ended (platf::streaming_will_start and streaming_will_stop). The log
  # misses some endings: an encoder failure stops a session without a line.
  param([string]$LogPath)
  $readable = $false
  foreach ($process in @(Get-Process -Name 'sunshine' -ErrorAction SilentlyContinue)) {
    try {
      if ($process.PriorityClass -eq [Diagnostics.ProcessPriorityClass]::High) { return $true }
      $readable = $true
    } catch {
      Write-ReGaLog "Could not read Sunshine's priority: $_" 'warning'
    }
  }
  if ($readable) { return $false }
  return ((Get-SunshineClientCount (Read-SharedTextFile $LogPath)) -gt 0)
}

function Read-SharedTextFile {
  # Sunshine keeps its log open for writing; open it with ReadWrite sharing.
  param([string]$Path)
  if (-not (Test-Path -LiteralPath $Path)) { return @() }
  $stream = New-Object IO.FileStream($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
  try {
    $reader = New-Object IO.StreamReader($stream)
    $text = $reader.ReadToEnd()
  } finally {
    $stream.Dispose()
  }
  return $text -split "`r?`n"
}

function Test-SteamDownloadActive {
  # Steam stages downloads and updates under steamapps\downloading. A file
  # written there recently means a download is running (not merely queued).
  param([string[]]$SteamAppsPaths, [datetime]$Now, [int]$WithinMinutes = 5)
  $since = $Now.AddMinutes(-$WithinMinutes)
  foreach ($steamApps in $SteamAppsPaths) {
    $downloading = Join-Path $steamApps 'downloading'
    if (-not (Test-Path -LiteralPath $downloading)) { continue }
    $recent = Get-ChildItem -LiteralPath $downloading -Recurse -File -Force -ErrorAction SilentlyContinue |
      Where-Object { $_.LastWriteTimeUtc -ge $since } |
      Select-Object -First 1
    if ($recent) { return $true }
  }
  return $false
}

function Test-RemoteShellActive {
  # Ansible and the app talk to the machine over WinRM, which runs each
  # remote shell in a wsmprovhost process. Don't stop under their feet.
  return [bool](Get-Process -Name 'wsmprovhost' -ErrorAction SilentlyContinue)
}

function Test-SameBoot {
  # Boot times written by one process and read by another; allow for
  # rounding.
  param($Recorded, [datetime]$Boot)
  $value = ConvertTo-UtcDate $Recorded
  if ($null -eq $value) { return $false }
  return ([Math]::Abs(($value - $Boot.ToUniversalTime()).TotalSeconds) -lt 60)
}

function Get-RestoreWaitState {
  # Pure: where this boot's library restore is. The boot task writes
  # restore.json as it starts, so until StartBy a missing or older record
  # means it hasn't started yet.
  #   running   the restore is copying
  #   starting  no record for this boot yet, but it may still start
  #   finished  it ended this boot (done, no-disk, failed or disk-full)
  #   none      no restore started this boot
  param($Restore, [datetime]$Boot, [datetime]$Now, [datetime]$StartBy)
  if (Test-SameBoot (Get-PropertyValue $Restore 'boot_utc') $Boot) {
    if ((Get-PropertyValue $Restore 'status') -eq 'running') { return 'running' }
    return 'finished'
  }
  if ($Now -lt $StartBy) { return 'starting' }
  return 'none'
}

function Get-RestoreWaitProblem {
  # Pure: why waiting for the restore didn't end with the games on the
  # library disk, or $null when it did.
  param([string]$Wait, [string]$Status, [int]$TimeoutMinutes, [string]$LogPath)
  if ($Wait -eq 'running') { return "The game library restore is still running after $TimeoutMinutes minutes." }
  if ($Wait -ne 'finished') { return 'No game library restore started on this boot.' }
  switch ($Status) {
    'done' { return $null }
    'no-disk' { return 'The machine has no game library disk.' }
    'disk-full' { return "The game library doesn't fit on the machine's library disk." }
    'failed' { return "The game library restore failed; see $LogPath on the machine." }
  }
  return "The game library restore ended as '$Status'."
}

function Get-IdleDecision {
  # Pure: given the saved state and the current signals, work out when the
  # machine was last active and whether it should stop now.
  param(
    $State,
    [datetime]$Now,
    [datetime]$BootTime,
    [bool]$Active,
    [int]$IdleMinutes,
    [bool]$IdleStopEnabled
  )
  $lastActive = ConvertTo-UtcDate (Get-PropertyValue $State 'last_active_utc')
  $boot = $BootTime.ToUniversalTime()
  # Every boot restarts the clock, so a machine that was idle when it
  # stopped gets the full idle time again after the next start.
  if ($null -eq $lastActive -or $lastActive -lt $boot) { $lastActive = $boot }
  if ($Active) { $lastActive = $Now.ToUniversalTime() }
  $idleFor = ($Now.ToUniversalTime() - $lastActive).TotalMinutes
  return [pscustomobject]@{
    LastActiveUtc = $lastActive
    IdleMinutes   = [Math]::Max(0, [Math]::Floor($idleFor))
    ShouldStop    = ($IdleStopEnabled -and -not $Active -and $idleFor -ge $IdleMinutes)
  }
}

# --- EC2 ----------------------------------------------------------------------

function Get-Ec2Identity {
  # IMDSv2 (the instance requires tokens). Returns $null off EC2.
  param([int]$TimeoutSec = 2)
  try {
    $token = Invoke-RestMethod -Method Put -Uri 'http://169.254.169.254/latest/api/token' `
      -Headers @{ 'X-aws-ec2-metadata-token-ttl-seconds' = '300' } -TimeoutSec $TimeoutSec -UseBasicParsing
    $doc = Invoke-RestMethod -Uri 'http://169.254.169.254/latest/dynamic/instance-identity/document' `
      -Headers @{ 'X-aws-ec2-metadata-token' = "$token" } -TimeoutSec $TimeoutSec -UseBasicParsing
    if (-not (Get-PropertyValue $doc 'instanceId')) { return $null }
    return $doc
  } catch {
    return $null
  }
}

# --- Game library on S3 -------------------------------------------------------

function Resolve-LibraryTarget {
  # Pure: bucket, region and S3 URI of the library. The app creates the
  # bucket rega-games-<account-id>; the account comes from the instance
  # identity unless the config names the bucket.
  param($Config, $Identity)
  $bucket = Get-PropertyValue $Config 'bucket' ''
  if (-not $bucket) {
    $account = Get-PropertyValue $Identity 'accountId' ''
    if (-not $account) { throw 'No library bucket configured and no EC2 identity to derive rega-games-<account-id> from.' }
    $bucket = "rega-games-$account"
  }
  $region = Get-PropertyValue $Config 'bucket_region' ''
  if (-not $region) { $region = Get-PropertyValue $Identity 'region' '' }
  if (-not $region) { throw 'No region for the library bucket.' }
  $prefix = (Get-PropertyValue $Config 'prefix' 'steam-library').Trim('/')
  return [pscustomobject]@{
    Bucket = $bucket
    Region = $region
    Prefix = $prefix
    Uri    = "s3://$bucket/$prefix/"
  }
}

function Get-SyncArguments {
  # Pure: arguments for "aws s3 sync". Excludes are relative to the synced
  # folder and use forward slashes on both sides.
  param([string]$Source, [string]$Destination, [string]$Region, [string[]]$Excludes, [switch]$Delete)
  $arguments = @('s3', 'sync', $Source, $Destination, '--region', $Region, '--no-progress', '--only-show-errors')
  foreach ($pattern in $Excludes) { $arguments += @('--exclude', $pattern) }
  if ($Delete) { $arguments += '--delete' }
  return , $arguments
}

function Invoke-Aws {
  # The exit code, or with -PassThru the code and the output lines.
  param([string]$AwsPath, [string[]]$Arguments, [switch]$PassThru)
  Write-ReGaLog ("aws " + ($Arguments -join ' '))
  # Windows PowerShell turns native stderr into terminating errors when
  # ErrorActionPreference is Stop.
  $ErrorActionPreference = 'Continue'
  $output = & $AwsPath @Arguments 2>&1
  $code = $LASTEXITCODE
  foreach ($line in $output) { Write-ReGaLog "aws: $line" 'debug' }
  if ($PassThru) { return [pscustomobject]@{ Code = $code; Output = @($output | ForEach-Object { "$_" }) } }
  return $code
}

function Test-DiskFullOutput {
  # Pure: whether AWS CLI output says the local disk filled up: ENOSPC, or
  # Windows' ERROR_DISK_FULL (112) or ERROR_HANDLE_DISK_FULL (39).
  param([string[]]$Output)
  return (@(@($Output) -match 'No space left on device|\[Errno 28\]|not enough space on the disk|\[WinError 112\]|disk is full|\[WinError 39\]').Count -gt 0)
}

function Get-AcfValue {
  # Pure: a top-level string value from a Steam .acf/.vdf file.
  param([string]$Text, [string]$Key)
  if ($Text -match ('"' + [regex]::Escape($Key) + '"\s+"([^"]*)"')) { return $Matches[1] }
  return $null
}

function Get-AcfDepots {
  # Pure: the depots an app manifest lists under InstalledDepots, as sorted
  # "depot:manifest" pairs joined by ';'. Uninstalling a DLC, a language or
  # an optional pack changes them without changing the build.
  param([string]$Text)
  $stack = [Collections.Generic.List[string]]::new()
  $depots = @{}
  $key = $null
  foreach ($token in [regex]::Matches("$Text", '"((?:[^"\\]|\\.)*)"|[{}]')) {
    $inDepot = $stack.Count -ge 2 -and $stack[$stack.Count - 2] -eq 'InstalledDepots'
    if ($token.Value -eq '{') {
      $stack.Add("$key")
      $key = $null
      if ($stack.Count -ge 2 -and $stack[$stack.Count - 2] -eq 'InstalledDepots') { $depots[$stack[$stack.Count - 1]] = '' }
    } elseif ($token.Value -eq '}') {
      if ($stack.Count -gt 0) { $stack.RemoveAt($stack.Count - 1) }
      $key = $null
    } elseif ($null -eq $key) {
      $key = $token.Groups[1].Value
    } else {
      if ($inDepot -and $key -eq 'manifest') { $depots[$stack[$stack.Count - 1]] = $token.Groups[1].Value }
      $key = $null
    }
  }
  return (@($depots.Keys | Sort-Object | ForEach-Object { '{0}:{1}' -f $_, $depots[$_] }) -join ';')
}

function Test-SafeFolderName {
  # Pure: a single folder name, so an S3 delete built from it can't reach
  # outside steamapps/common.
  param([string]$Name)
  if (-not $Name -or $Name.Trim() -eq '' -or $Name -eq '.' -or $Name -eq '..') { return $false }
  return ($Name.IndexOfAny([char[]]'\/:*?"<>|') -lt 0)
}

function ConvertTo-AppTable {
  # appid -> installdir as a hashtable, from a hashtable or from JSON.
  param($Value)
  $table = @{}
  if ($null -eq $Value) { return $table }
  if ($Value -is [hashtable]) {
    foreach ($key in $Value.Keys) { $table["$key"] = "$($Value[$key])" }
  } else {
    foreach ($p in $Value.PSObject.Properties) { $table[$p.Name] = "$($p.Value)" }
  }
  return $table
}

function Get-ManifestValues {
  # appid -> the value of Key, for every app manifest that parses.
  param([string]$SteamAppsPath, [string]$Key)
  $values = @{}
  if (-not (Test-Path -LiteralPath $SteamAppsPath)) { return $values }
  foreach ($file in @(Get-ChildItem -LiteralPath $SteamAppsPath -Filter 'appmanifest_*.acf' -File -ErrorAction SilentlyContinue)) {
    try { $text = [IO.File]::ReadAllText($file.FullName) } catch { continue }
    $id = Get-AcfValue $text 'appid'
    $value = Get-AcfValue $text $Key
    if ($id -and $null -ne $value) { $values[$id] = $value }
  }
  return $values
}

function Get-SteamApps {
  # appid -> installdir for every app manifest in a steamapps folder.
  param([string]$SteamAppsPath)
  $apps = @{}
  $dirs = Get-ManifestValues $SteamAppsPath 'installdir'
  foreach ($id in $dirs.Keys) {
    if (Test-SafeFolderName $dirs[$id]) { $apps[$id] = $dirs[$id] }
  }
  return $apps
}

function Get-ManifestStamps {
  # appid -> size and write time of each app manifest file (whether it
  # parses or not) and of its workshop manifest. Steam rewrites them
  # whenever the app or its workshop items change.
  param([string]$SteamAppsPath)
  $stamps = @{}
  if (-not (Test-Path -LiteralPath $SteamAppsPath)) { return $stamps }
  foreach ($file in @(Get-ChildItem -LiteralPath $SteamAppsPath -Filter 'appmanifest_*.acf' -File -ErrorAction SilentlyContinue)) {
    if ($file.Name -match '^appmanifest_(\d+)\.acf$') {
      $id = $Matches[1]
      $stamp = '{0}:{1}' -f $file.Length, $file.LastWriteTimeUtc.Ticks
      $workshop = Get-Item -LiteralPath (Join-Path (Join-Path $SteamAppsPath 'workshop') "appworkshop_$id.acf") -ErrorAction SilentlyContinue
      if ($workshop) { $stamp += '|{0}:{1}' -f $workshop.Length, $workshop.LastWriteTimeUtc.Ticks }
      $stamps[$id] = $stamp
    }
  }
  return $stamps
}

function Get-RemovedApps {
  # Pure: apps this disk knew (restored or uploaded) whose manifest file is
  # gone. A manifest that exists but doesn't parse (Steam may be rewriting
  # it) is not a removal. DeleteFolder is false when an app still on disk
  # uses the same folder.
  param([hashtable]$Known, [hashtable]$Current, [string[]]$Present)
  $inUse = @()
  foreach ($id in $Current.Keys) { $inUse += "$($Current[$id])".ToLowerInvariant() }
  foreach ($id in @($Present)) { if ($Known.ContainsKey($id)) { $inUse += "$($Known[$id])".ToLowerInvariant() } }
  $removed = @()
  foreach ($id in @($Known.Keys | Sort-Object)) {
    if (@($Present) -contains $id) { continue }
    $dir = $Known[$id]
    $removed += [pscustomobject]@{
      AppId        = $id
      InstallDir   = $dir
      DeleteFolder = ((Test-SafeFolderName $dir) -and ($inUse -notcontains $dir.ToLowerInvariant()))
    }
  }
  return , $removed
}

function Test-UploadAllowed {
  # Pure: whether this disk may overwrite an app's copy in S3. Not when S3
  # holds a build this disk neither started from nor has now: another
  # machine uploaded it since, and mirroring would mix the two builds.
  param([string]$S3Build, [string]$BaseBuild, [string]$LocalBuild)
  if (-not $S3Build) { return $true }
  return ($S3Build -eq $BaseBuild -or $S3Build -eq $LocalBuild)
}

function Get-S3Text {
  # A small S3 object as text. Found is false when it doesn't exist; Ok is
  # false when S3 couldn't be asked.
  param([string]$AwsPath, [string]$Uri, [string]$Region)
  $ErrorActionPreference = 'Continue'
  $output = & $AwsPath s3 cp $Uri - --region $Region --only-show-errors 2>&1
  $code = $LASTEXITCODE
  $lines = @($output | ForEach-Object { "$_" })
  if ($code -eq 0) { return [pscustomobject]@{ Ok = $true; Found = $true; Text = ($lines -join "`n") } }
  $message = $lines -join ' '
  if ($message -match '\(404\)|NoSuchKey|Not Found') { return [pscustomobject]@{ Ok = $true; Found = $false; Text = '' } }
  Write-ReGaLog "Could not read ${Uri}: $message" 'error'
  return [pscustomobject]@{ Ok = $false; Found = $false; Text = '' }
}

function Test-ChangedSince {
  # Whether anything in these folders was written or created after Since.
  # Files restored from S3 carry S3's older timestamps, and adding or
  # deleting a file updates its folder's write time.
  param([string[]]$Paths, [datetime]$Since)
  foreach ($path in $Paths) {
    $root = Get-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
    if (-not $root) { continue }
    if ($root.LastWriteTimeUtc -gt $Since -or $root.CreationTimeUtc -gt $Since) { return $true }
    $hit = Get-ChildItem -LiteralPath $path -Recurse -Force -ErrorAction SilentlyContinue |
      Where-Object { $_.LastWriteTimeUtc -gt $Since -or $_.CreationTimeUtc -gt $Since } |
      Select-Object -First 1
    if ($hit) { return $true }
  }
  return $false
}

function Get-LibrarySnapshot {
  # What the marker records about the games on a disk.
  param([string]$SteamAppsPath)
  return [pscustomobject]@{
    apps   = (Get-SteamApps $SteamAppsPath)
    stamps = (Get-ManifestStamps $SteamAppsPath)
    builds = (Get-ManifestValues $SteamAppsPath 'buildid')
  }
}

function Invoke-LibraryUpload {
  # Other machines in the account share the library, so nothing here
  # mirrors deletes across the whole library, and a game is only uploaded
  # when this disk changed it:
  #   1. copy libraryfolder.vdf;
  #   2. for each game this disk installed, updated or wrote files into
  #      since the restore or its last upload: upload its folders, then its
  #      manifest, so S3 never has a manifest ahead of its files;
  #   3. delete the games whose manifest file this disk had and lost.
  # Games are looked at when their manifests changed, or all of them with
  # -All (before a stop, so saves and mods written into game folders are
  # kept, and a game another machine removed goes back while this disk
  # still has it). A game's folder is mirrored, deletes included, only when
  # S3 holds another build, other installed depots or nothing; otherwise
  # files are only added or replaced, so two machines' saves both stay. Workshop content always
  # mirrors its manifest. A game is left alone, for upload and for removal,
  # when S3 holds a build another machine uploaded. Only a disk restored
  # from S3 this session uploads at all: its marker records the games it
  # started with.
  # Returns Ok, the apps skipped for another machine's build and the apps
  # that failed.
  param([string]$AwsPath, [string]$LibraryPath, [string]$MarkerPath, $Target, [switch]$All)
  $skipped = @()
  $failed = @()
  if (-not (Test-Path -LiteralPath $MarkerPath)) {
    Write-ReGaLog "Skipping upload: $LibraryPath was not restored from S3 this session." 'warning'
    return [pscustomobject]@{ Ok = $false; Skipped = $skipped; Failed = $failed }
  }
  $steamApps = Join-Path $LibraryPath 'steamapps'
  $workshop = Join-Path $steamApps 'workshop'
  $uri = $Target.Uri
  $region = $Target.Region
  $restored = Read-JsonFile $MarkerPath
  $restoredUtc = ConvertTo-UtcDate (Get-PropertyValue $restored 'restored_utc')
  if ($null -eq $restoredUtc) { $restoredUtc = [datetime]::MinValue }
  $known = ConvertTo-AppTable (Get-PropertyValue $restored 'apps')
  $knownStamps = ConvertTo-AppTable (Get-PropertyValue $restored 'stamps')
  $knownBuilds = ConvertTo-AppTable (Get-PropertyValue $restored 'builds')
  $knownSynced = ConvertTo-AppTable (Get-PropertyValue $restored 'synced')
  $current = Get-SteamApps $steamApps
  $stamps = Get-ManifestStamps $steamApps
  $ok = $true

  # Listing the whole shared prefix for one small file would be slow.
  $folderFile = Join-Path $LibraryPath 'libraryfolder.vdf'
  if (Test-Path -LiteralPath $folderFile) {
    $arguments = @('s3', 'cp', $folderFile, "${uri}libraryfolder.vdf", '--region', $region, '--only-show-errors')
    if ((Invoke-Aws -AwsPath $AwsPath -Arguments $arguments) -ne 0) { $ok = $false; $failed += 'libraryfolder.vdf' }
  }

  foreach ($id in @($current.Keys | Sort-Object)) {
    if (-not $All -and $stamps[$id] -eq $knownStamps[$id]) { continue }
    $dir = $current[$id]
    $manifest = Join-Path $steamApps "appmanifest_$id.acf"
    try { $localText = [IO.File]::ReadAllText($manifest) } catch { $ok = $false; $failed += $id; continue }
    $localBuild = Get-AcfValue $localText 'buildid'
    # Local folder, S3 prefix, and whether it always mirrors. Soundtracks
    # install under music instead of common.
    $folders = @(
      @((Join-Path (Join-Path $steamApps 'common') $dir), "${uri}steamapps/common/$dir/", $false),
      @((Join-Path (Join-Path $steamApps 'music') $dir), "${uri}steamapps/music/$dir/", $false),
      @((Join-Path (Join-Path $workshop 'content') $id), "${uri}steamapps/workshop/content/$id/", $true)
    )
    $since = ConvertTo-UtcDate $knownSynced[$id]
    if ($null -eq $since) { $since = $restoredUtc }
    $changed = (-not $known.ContainsKey($id)) -or ($localBuild -ne $knownBuilds[$id]) -or
      (Test-ChangedSince -Paths @($folders | ForEach-Object { $_[0] }) -Since $since)
    $remote = $null
    if (-not $changed -and $All) {
      # Another machine may have uninstalled it; this disk still has it.
      $remote = Get-S3Text -AwsPath $AwsPath -Uri "${uri}steamapps/appmanifest_$id.acf" -Region $region
      if (-not $remote.Ok) { $ok = $false; $failed += $id; continue }
      if (-not $remote.Found) {
        Write-ReGaLog "Uploading app $id ($dir) again: another machine removed it from $uri."
        $changed = $true
      }
    }
    if (-not $changed) {
      # Only the manifest was rewritten, as Steam does at its first start.
      $knownStamps[$id] = $stamps[$id]
      continue
    }
    if ($null -eq $remote) {
      $remote = Get-S3Text -AwsPath $AwsPath -Uri "${uri}steamapps/appmanifest_$id.acf" -Region $region
      if (-not $remote.Ok) { $ok = $false; $failed += $id; continue }
    }
    $s3Build = Get-AcfValue $remote.Text 'buildid'
    if (-not (Test-UploadAllowed -S3Build $s3Build -BaseBuild $knownBuilds[$id] -LocalBuild $localBuild)) {
      Write-ReGaLog "Not uploading app $id ($dir): S3 has build $s3Build from another machine, this disk has $localBuild." 'warning'
      $skipped += $id
      continue
    }
    # Known from the first attempt on, so an uninstall cleans up a partial
    # upload too.
    if (-not $known.ContainsKey($id)) { $known[$id] = $dir }
    $started = [datetime]::UtcNow
    $appOk = $true
    # The same build and depots as S3: whatever this disk lacks, Steam
    # didn't remove.
    $sameContent = ($s3Build -eq $localBuild) -and ((Get-AcfDepots $remote.Text) -eq (Get-AcfDepots $localText))
    foreach ($folder in $folders) {
      if (-not (Test-Path -LiteralPath $folder[0])) { continue }
      $mirror = $folder[2] -or -not $sameContent
      $arguments = Get-SyncArguments -Source $folder[0] -Destination $folder[1] -Region $region -Excludes @() -Delete:$mirror
      if ((Invoke-Aws -AwsPath $AwsPath -Arguments $arguments) -ne 0) { $appOk = $false }
    }
    # A leading comma keeps a single pair from being flattened.
    $files = @(, @($manifest, "${uri}steamapps/appmanifest_$id.acf"))
    $workshopManifest = Join-Path $workshop "appworkshop_$id.acf"
    if (Test-Path -LiteralPath $workshopManifest) { $files += , @($workshopManifest, "${uri}steamapps/workshop/appworkshop_$id.acf") }
    foreach ($pair in $files) {
      if (-not $appOk) { break }
      $arguments = @('s3', 'cp', $pair[0], $pair[1], '--region', $region, '--only-show-errors')
      if ((Invoke-Aws -AwsPath $AwsPath -Arguments $arguments) -ne 0) { $appOk = $false }
    }
    if ($appOk) {
      $known[$id] = $dir
      $knownStamps[$id] = $stamps[$id]
      $knownBuilds[$id] = $localBuild
      $knownSynced[$id] = ConvertTo-IsoString $started
    } else {
      $ok = $false
      $failed += $id
    }
  }

  foreach ($app in (Get-RemovedApps -Known $known -Current $current -Present @($stamps.Keys))) {
    $id = $app.AppId
    $remote = Get-S3Text -AwsPath $AwsPath -Uri "${uri}steamapps/appmanifest_$id.acf" -Region $region
    if (-not $remote.Ok) { $ok = $false; $failed += $id; continue }
    $s3Build = Get-AcfValue $remote.Text 'buildid'
    $forget = $true
    if ($remote.Found -and $s3Build -ne $knownBuilds[$id]) {
      # Another machine uploaded it since; it is theirs now.
      Write-ReGaLog "Not removing app ${id}: S3 has build $s3Build from another machine." 'warning'
    } else {
      Write-ReGaLog "Removing uninstalled app $id ($($app.InstallDir)) from $uri."
      # The manifest goes first: a half-finished removal reads as uninstalled.
      $deletes = @(
        @('s3', 'rm', "${uri}steamapps/appmanifest_$id.acf"),
        @('s3', 'rm', "${uri}steamapps/workshop/appworkshop_$id.acf"),
        @('s3', 'rm', "${uri}steamapps/workshop/content/$id/", '--recursive')
      )
      if ($app.DeleteFolder) {
        $deletes += , @('s3', 'rm', "${uri}steamapps/common/$($app.InstallDir)/", '--recursive')
        $deletes += , @('s3', 'rm', "${uri}steamapps/music/$($app.InstallDir)/", '--recursive')
      }
      foreach ($delete in $deletes) {
        if ((Invoke-Aws -AwsPath $AwsPath -Arguments (@($delete) + @('--region', $region, '--only-show-errors'))) -ne 0) { $forget = $false }
      }
    }
    if ($forget) {
      $known.Remove($id)
      $knownStamps.Remove($id)
      $knownBuilds.Remove($id)
      $knownSynced.Remove($id)
    } else {
      $ok = $false
      $failed += $id
    }
  }

  Write-JsonFile $MarkerPath ([pscustomobject]@{
    restored_utc = (Get-PropertyValue $restored 'restored_utc')
    source       = $uri
    apps         = $known
    stamps       = $knownStamps
    builds       = $knownBuilds
    synced       = $knownSynced
    uploaded_utc = (ConvertTo-IsoString ([datetime]::UtcNow))
  })
  if ($ok) {
    Write-ReGaLog "Uploaded $LibraryPath to $uri."
  } else {
    Write-ReGaLog "Upload to $uri was incomplete (failed: $($failed -join ', ')); the rest is retried at the next change or stop." 'error'
  }
  return [pscustomobject]@{ Ok = $ok; Skipped = $skipped; Failed = $failed }
}

function Get-LibraryFingerprint {
  # Steam rewrites an app's manifest (and its workshop manifest) when a
  # download or update finishes and deletes it on uninstall, so the
  # manifests change exactly when there is something worth uploading.
  param([string]$SteamAppsPath)
  if (-not (Test-Path -LiteralPath $SteamAppsPath)) { return '' }
  $files = @(Get-ChildItem -LiteralPath $SteamAppsPath -Filter 'appmanifest_*.acf' -File -ErrorAction SilentlyContinue) +
    @(Get-ChildItem -LiteralPath (Join-Path $SteamAppsPath 'workshop') -Filter 'appworkshop_*.acf' -File -ErrorAction SilentlyContinue)
  $parts = $files |
    Sort-Object Name |
    ForEach-Object { '{0}:{1}:{2}' -f $_.Name, $_.Length, $_.LastWriteTimeUtc.Ticks }
  return ($parts -join '|')
}

function Test-LibraryVolume {
  # Only restore into the disk the boot script formatted for the library.
  param([string]$LibraryPath)
  $drive = [IO.Path]::GetPathRoot($LibraryPath).TrimEnd('\').TrimEnd(':')
  $volume = Get-Volume -DriveLetter $drive -ErrorAction SilentlyContinue
  return ($null -ne $volume -and $volume.FileSystemLabel -eq 'ReGaLibrary')
}

function Get-RestoredMarkerPath {
  # The marker lives on the library disk itself, outside the synced folder:
  # a new, empty disk has no marker, so it is never mirrored over S3.
  param([string]$LibraryPath)
  return (Join-Path ([IO.Path]::GetPathRoot($LibraryPath)) '.rega-restored.json')
}

# --- Steam library folder -----------------------------------------------------

function ConvertTo-VdfString {
  param([string]$Value)
  return $Value.Replace('\', '\\').Replace('"', '\"')
}

function New-LibraryFolderVdf {
  # Pure: the libraryfolder.vdf Steam keeps at the root of each library.
  param([string]$ContentId)
  return "`"libraryfolder`"`n{`n`t`"contentid`"`t`t`"$ContentId`"`n`t`"label`"`t`t`"`"`n}`n"
}

function Get-VdfContentId {
  param([string]$Text)
  if ($Text -match '"contentid"\s+"(\d+)"') { return $Matches[1] }
  return $null
}

function New-SteamContentId {
  $bytes = New-Object byte[] 8
  $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
  try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
  return ([BitConverter]::ToUInt64($bytes, 0) -band [UInt64]::Parse('9223372036854775807')).ToString()
}

function Add-SteamLibraryFolder {
  # Pure: returns libraryfolders.vdf text that lists LibraryPath, or $null
  # when it is already listed. Text is the current file ('' when missing).
  param([string]$Text, [string]$SteamPath, [string]$LibraryPath, [string]$ContentId)
  $entry = {
    param([string]$Index, [string]$Path, [string]$Id)
    "`t`"$Index`"`n`t{`n`t`t`"path`"`t`t`"$(ConvertTo-VdfString $Path)`"`n`t`t`"label`"`t`t`"`"`n`t`t`"contentid`"`t`t`"$Id`"`n`t`t`"totalsize`"`t`t`"0`"`n`t`t`"update_clean_bytes_tally`"`t`t`"0`"`n`t`t`"time_last_update_verified`"`t`t`"0`"`n`t`t`"apps`"`n`t`t{`n`t`t}`n`t}`n"
  }
  if (-not $Text -or $Text.Trim() -eq '') {
    return "`"libraryfolders`"`n{`n" + (& $entry '0' $SteamPath (New-SteamContentId)) + (& $entry '1' $LibraryPath $ContentId) + "}`n"
  }
  $wanted = (ConvertTo-VdfString $LibraryPath.TrimEnd('\')).ToLowerInvariant()
  foreach ($m in [regex]::Matches($Text, '"path"\s+"([^"]*)"')) {
    if ($m.Groups[1].Value.TrimEnd('\').ToLowerInvariant() -eq $wanted) { return $null }
  }
  $indexes = @([regex]::Matches($Text, '(?m)^\s*"(\d+)"\s*$') | ForEach-Object { [int]$_.Groups[1].Value })
  $next = 0
  if ($indexes.Count -gt 0) { $next = ($indexes | Measure-Object -Maximum).Maximum + 1 }
  $close = $Text.LastIndexOf('}')
  if ($close -lt 0) { throw 'libraryfolders.vdf has no closing brace.' }
  return $Text.Substring(0, $close) + (& $entry "$next" $LibraryPath $ContentId) + $Text.Substring($close)
}

Export-ModuleMember -Function *
