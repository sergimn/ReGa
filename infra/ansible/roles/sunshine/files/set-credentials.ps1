# Sets Sunshine's web UI login unless sunshine_state.json already holds it.
# Sunshine stores hex(sha256(password + salt)) with the digest bytes in
# reverse order; both orders are accepted in case that changes.
param(
  [Parameter(Mandatory = $true)][string]$SunshinePath,
  [Parameter(Mandatory = $true)][string]$Username,
  [Parameter(Mandatory = $true)][string]$Password
)
$ErrorActionPreference = 'Stop'
$Ansible.Changed = $false

$statePath = Join-Path $SunshinePath 'config\sunshine_state.json'
$current = $false
if (Test-Path -LiteralPath $statePath) {
  $state = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
  $names = $state.PSObject.Properties.Name
  if ($names -contains 'username' -and $names -contains 'password' -and $names -contains 'salt' -and $state.username -eq $Username) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $digest = $sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Password + $state.salt)) } finally { $sha.Dispose() }
    $forward = -join ($digest | ForEach-Object { $_.ToString('X2') })
    [Array]::Reverse($digest)
    $reversed = -join ($digest | ForEach-Object { $_.ToString('X2') })
    $current = ($state.password -eq $forward -or $state.password -eq $reversed)
  }
}

if (-not $current) {
  $exe = Join-Path $SunshinePath 'sunshine.exe'
  $process = Start-Process -FilePath $exe -ArgumentList @('--creds', $Username, $Password) -WorkingDirectory $SunshinePath -Wait -PassThru -WindowStyle Hidden
  if ($process.ExitCode -ne 0) { throw "sunshine.exe --creds exited with $($process.ExitCode)" }
  $Ansible.Changed = $true
}
