# Writes the WireGuard server config (keeping the private key it made the
# first time) and runs it as the tunnel service WireGuardTunnel$rega.
param(
  [Parameter(Mandatory = $true)][string]$WireGuardPath,
  [Parameter(Mandatory = $true)][string]$ConfigDir,
  [Parameter(Mandatory = $true)][int]$ListenPort,
  [Parameter(Mandatory = $true)][string]$Address,
  [Parameter(Mandatory = $true)][int]$Mtu,
  [Parameter(Mandatory = $true)][string]$ClientAddress,
  [string]$ClientPublicKey = ''
)
$ErrorActionPreference = 'Stop'
$Ansible.Changed = $false
$wg = Join-Path $WireGuardPath 'wg.exe'
$utf8 = New-Object Text.UTF8Encoding($false)

if (-not (Test-Path -LiteralPath $ConfigDir)) {
  New-Item -ItemType Directory -Path $ConfigDir | Out-Null
  $Ansible.Changed = $true
}
# SYSTEM and Administrators only, not inherited from ProgramData.
& icacls.exe $ConfigDir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' /Q | Out-Null

function Get-Key {
  param([string]$Name)
  $path = Join-Path $ConfigDir $Name
  if (-not (Test-Path -LiteralPath $path)) {
    $key = (& $wg genkey | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $key) { throw "wg genkey failed" }
    [IO.File]::WriteAllText($path, $key, $utf8)
    $Ansible.Changed = $true
  }
  return [IO.File]::ReadAllText($path).Trim()
}

function Get-PublicKey {
  param([string]$PrivateKey)
  $public = ($PrivateKey | & $wg pubkey | Out-String).Trim()
  if ($LASTEXITCODE -ne 0 -or -not $public) { throw "wg pubkey failed" }
  return $public
}

$serverPrivate = Get-Key 'server.key'
$clientPrivate = ''
if (-not $ClientPublicKey) {
  $clientPrivate = Get-Key 'client.key'
  $ClientPublicKey = Get-PublicKey $clientPrivate
}

$config = @"
[Interface]
PrivateKey = $serverPrivate
ListenPort = $ListenPort
Address = $Address
MTU = $Mtu

[Peer]
PublicKey = $ClientPublicKey
AllowedIPs = $ClientAddress
"@

$confPath = Join-Path $ConfigDir 'rega.conf'
$existing = ''
if (Test-Path -LiteralPath $confPath) { $existing = [IO.File]::ReadAllText($confPath) }
$configChanged = ($existing -ne $config)
if ($configChanged) {
  [IO.File]::WriteAllText($confPath, $config, $utf8)
  $Ansible.Changed = $true
}

$serviceName = 'WireGuardTunnel$rega'
$service = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
if (-not $service) {
  & (Join-Path $WireGuardPath 'wireguard.exe') /installtunnelservice $confPath
  if ($LASTEXITCODE -ne 0) { throw "wireguard /installtunnelservice exited with $LASTEXITCODE" }
  $Ansible.Changed = $true
} elseif ($configChanged) {
  Restart-Service -Name $serviceName
} elseif ($service.Status -ne 'Running') {
  Start-Service -Name $serviceName
  $Ansible.Changed = $true
}

# The tunnel service starts asynchronously after installation.
$deadline = (Get-Date).AddSeconds(60)
do {
  $service = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
  if ($service -and $service.Status -eq 'Running') { break }
  Start-Sleep -Seconds 2
} while ((Get-Date) -lt $deadline)
if (-not $service -or $service.Status -ne 'Running') { throw "$serviceName is not running" }

$Ansible.Result = @{
  server_public_key  = (Get-PublicKey $serverPrivate)
  client_private_key = $clientPrivate
}
