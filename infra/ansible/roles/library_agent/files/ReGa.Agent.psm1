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
  $tmp = "$Path.tmp"
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
  # Sunshine rotates its log at every start and writes "CLIENT CONNECTED" and
  # "CLIENT DISCONNECTED" when a Moonlight control stream opens or closes.
  # Count them after the last startup line, so a crash can't leave a session
  # open forever.
  param([string[]]$Lines)
  $connected = 0
  foreach ($line in $Lines) {
    if ($line -match ': Sunshine version: ') {
      $connected = 0
    } elseif ($line -match 'CLIENT CONNECTED\s*$') {
      $connected++
    } elseif ($line -match 'CLIENT DISCONNECTED\s*$') {
      $connected = [Math]::Max(0, $connected - 1)
    }
  }
  return $connected
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
  param([string]$AwsPath, [string[]]$Arguments)
  Write-ReGaLog ("aws " + ($Arguments -join ' '))
  # Windows PowerShell turns native stderr into terminating errors when
  # ErrorActionPreference is Stop.
  $ErrorActionPreference = 'Continue'
  $output = & $AwsPath @Arguments 2>&1
  $code = $LASTEXITCODE
  foreach ($line in $output) { Write-ReGaLog "aws: $line" 'debug' }
  return $code
}

function Get-LibraryFingerprint {
  # Steam rewrites an app's manifest when a download or update finishes and
  # deletes it on uninstall, so the manifests change exactly when there is
  # something worth uploading.
  param([string]$SteamAppsPath)
  if (-not (Test-Path -LiteralPath $SteamAppsPath)) { return '' }
  $parts = Get-ChildItem -LiteralPath $SteamAppsPath -Filter 'appmanifest_*.acf' -File -ErrorAction SilentlyContinue |
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
