# Returns the SHA-256 of Sunshine's self-signed certificate (of its DER
# bytes, in lowercase hex). The app pins it when it reaches Sunshine over
# the internet rather than through WireGuard. Sunshine makes the
# certificate the first time it starts, so this waits for it.
param(
  [Parameter(Mandatory = $true)][string]$CertificatePath,
  [int]$TimeoutSeconds = 120
)
$ErrorActionPreference = 'Stop'
$Ansible.Changed = $false

$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$cert = $null
while (-not $cert) {
  try {
    # X509Certificate2 reads PEM as well as DER, on .NET Framework too.
    $cert = New-Object Security.Cryptography.X509Certificates.X509Certificate2 -ArgumentList $CertificatePath
  } catch {
    # Not there yet, or still being written.
    if ((Get-Date) -ge $deadline) { throw "Sunshine hasn't made its certificate at ${CertificatePath}: $_" }
    Start-Sleep -Seconds 2
  }
}
$sha = [Security.Cryptography.SHA256]::Create()
try { $digest = $sha.ComputeHash($cert.RawData) } finally { $sha.Dispose() }
$Ansible.Result = -join ($digest | ForEach-Object { $_.ToString('x2') })
