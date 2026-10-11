<#
.SYNOPSIS
  Sets up the GPU on the first boot as a GPU instance type.

.DESCRIPTION
  The machine is created on a CPU type and resized to g4dn later, so the
  NVIDIA driver can only be installed once the GPU is attached. A boot task
  runs this script on every start:

  1. No NVIDIA GPU: nothing to do (CPU phase).
  2. GPU without driver: download the NVIDIA gaming driver from AWS's
     nvidia-gaming bucket (the instance role needs read access), install it,
     license it for gaming and reboot.
  3. Driver present: install the virtual display (VDD) so Sunshine has a
     monitor to capture, then disable the EC2 basic display adapter so the
     virtual display is the only, primary one.

  -Action VirtualDisplay installs only the virtual display, regardless of
  the GPU. CI uses it on GPU-less Windows runners.

  Prints one JSON line with the outcome.
#>
[CmdletBinding()]
param(
  [ValidateSet('Auto', 'VirtualDisplay', 'Status')]
  [string]$Action = 'Auto',
  [switch]$NoReboot,
  [string]$ConfigPath = (Join-Path $PSScriptRoot 'gpu-config.json')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0

$config = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
$logPath = Join-Path $config.log_dir 'gpu-setup.log'
$statePath = Join-Path $config.state_dir 'gpu-setup.json'
$vddHardwareId = 'Root\MttVDD'

function Write-GpuLog {
  param([string]$Message, [string]$Level = 'info')
  $line = '{0} [{1}] {2}' -f [datetime]::UtcNow.ToString('o'), $Level, $Message
  New-Item -ItemType Directory -Path (Split-Path -Parent $logPath) -Force | Out-Null
  Add-Content -LiteralPath $logPath -Value $line -Encoding UTF8
}

function Get-NvidiaGpu {
  return @(Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
    Where-Object { $_.InstanceId -like 'PCI\VEN_10DE*' -and $_.Class -in @('Display', '3D Video Controller', '') })
}

function Test-NvidiaDriver {
  # Before the driver is installed the T4 shows up without a driver or with
  # Microsoft's basic display driver.
  $drivers = @(Get-CimInstance Win32_PnPSignedDriver -ErrorAction SilentlyContinue |
    Where-Object { $_.DeviceID -like 'PCI\VEN_10DE*' -and $_.DriverProviderName -like 'NVIDIA*' })
  return ($drivers.Count -gt 0)
}

function Get-VirtualDisplay {
  return @(Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
    Where-Object { $_.HardwareID -contains $vddHardwareId })
}

function Read-State {
  if (Test-Path -LiteralPath $statePath) { return Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json }
  return [pscustomobject]@{ driver_attempts = 0 }
}

function Save-State {
  param($State)
  New-Item -ItemType Directory -Path (Split-Path -Parent $statePath) -Force | Out-Null
  $State | ConvertTo-Json | Set-Content -LiteralPath $statePath -Encoding UTF8
}

function Set-GamingLicense {
  # The gaming driver checks this marketplace flag and certificate; see
  # "NVIDIA gaming drivers" in the EC2 user guide.
  $key = 'HKLM:\SOFTWARE\NVIDIA Corporation\Global'
  New-Item -Path $key -Force | Out-Null
  New-ItemProperty -Path $key -Name 'vGamingMarketplace' -PropertyType DWord -Value 2 -Force | Out-Null
  $cert = Join-Path $env:PUBLIC 'Documents\GridSwCert.txt'
  if (-not (Test-Path -LiteralPath $cert)) {
    Invoke-WebRequest -UseBasicParsing -Uri $config.nvidia.certificate_url -OutFile $cert
  }
}

function Install-NvidiaDriver {
  $work = Join-Path $config.work_dir 'nvidia'
  if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
  New-Item -ItemType Directory -Path $work -Force | Out-Null
  Write-GpuLog "Downloading the NVIDIA gaming driver from $($config.nvidia.s3_uri)"
  $ErrorActionPreference = 'Continue'
  $output = & $config.aws_path s3 cp $config.nvidia.s3_uri $work --recursive --region $config.nvidia.region --only-show-errors 2>&1
  $code = $LASTEXITCODE
  $ErrorActionPreference = 'Stop'
  if ($code -ne 0) {
    throw "Driver download failed (exit $code): $output. ReGaInstanceRole needs s3:ListBucket and s3:GetObject on arn:aws:s3:::nvidia-gaming (see infra/ansible/README.md)."
  }
  Get-ChildItem -LiteralPath $work -Recurse -Filter *.zip | ForEach-Object {
    Expand-Archive -LiteralPath $_.FullName -DestinationPath (Join-Path $_.DirectoryName $_.BaseName) -Force
  }
  $installer = Get-ChildItem -LiteralPath $work -Recurse -Filter *.exe | Sort-Object Length -Descending | Select-Object -First 1
  if (-not $installer) { throw "No driver installer found under $work." }
  Write-GpuLog "Installing $($installer.Name)"
  $process = Start-Process -FilePath $installer.FullName -ArgumentList @('-s', '-noreboot', '-noeula', '-clean') -Wait -PassThru
  Write-GpuLog "Driver installer exited with $($process.ExitCode)"
  Set-GamingLicense
  if (-not (Test-NvidiaDriver)) { throw "The NVIDIA driver is still missing after the installer exited with $($process.ExitCode)." }
}

$deviceInstaller = @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class ReGaDeviceInstaller {
  [StructLayout(LayoutKind.Sequential)]
  public struct SP_DEVINFO_DATA {
    public int cbSize;
    public Guid ClassGuid;
    public int DevInst;
    public IntPtr Reserved;
  }

  [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern bool SetupDiGetINFClassW(string infName, out Guid classGuid, StringBuilder className, int classNameSize, out int requiredSize);

  [DllImport("setupapi.dll", SetLastError = true)]
  static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid classGuid, IntPtr hwndParent);

  [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern bool SetupDiCreateDeviceInfoW(IntPtr set, string deviceName, ref Guid classGuid, string description, IntPtr hwndParent, int flags, ref SP_DEVINFO_DATA data);

  [DllImport("setupapi.dll", SetLastError = true)]
  static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set, ref SP_DEVINFO_DATA data, int property, byte[] buffer, int size);

  [DllImport("setupapi.dll", SetLastError = true)]
  static extern bool SetupDiCallClassInstaller(int installFunction, IntPtr set, ref SP_DEVINFO_DATA data);

  [DllImport("setupapi.dll", SetLastError = true)]
  static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);

  [DllImport("newdev.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr hwndParent, string hardwareId, string infPath, int flags, out bool rebootRequired);

  const int DICD_GENERATE_ID = 1;
  const int SPDRP_HARDWAREID = 1;
  const int DIF_REGISTERDEVICE = 0x19;
  const int INSTALLFLAG_FORCE = 1;

  // What "devcon install <inf> <hwid>" does: create a root-enumerated device
  // with the hardware id, then install the driver from the INF on it.
  public static bool Install(string infPath, string hardwareId) {
    Guid classGuid;
    StringBuilder className = new StringBuilder(256);
    int required;
    if (!SetupDiGetINFClassW(infPath, out classGuid, className, className.Capacity, out required))
      throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiGetINFClass");
    IntPtr set = SetupDiCreateDeviceInfoList(ref classGuid, IntPtr.Zero);
    if (set == new IntPtr(-1))
      throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCreateDeviceInfoList");
    try {
      SP_DEVINFO_DATA data = new SP_DEVINFO_DATA();
      data.cbSize = Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
      if (!SetupDiCreateDeviceInfoW(set, className.ToString(), ref classGuid, null, IntPtr.Zero, DICD_GENERATE_ID, ref data))
        throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCreateDeviceInfo");
      byte[] ids = Encoding.Unicode.GetBytes(hardwareId + "\0\0");
      if (!SetupDiSetDeviceRegistryPropertyW(set, ref data, SPDRP_HARDWAREID, ids, ids.Length))
        throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiSetDeviceRegistryProperty");
      if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, ref data))
        throw new Win32Exception(Marshal.GetLastWin32Error(), "SetupDiCallClassInstaller");
    } finally {
      SetupDiDestroyDeviceInfoList(set);
    }
    bool reboot;
    if (!UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hardwareId, infPath, INSTALLFLAG_FORCE, out reboot))
      throw new Win32Exception(Marshal.GetLastWin32Error(), "UpdateDriverForPlugAndPlayDevices");
    return reboot;
  }
}
'@

function Install-VirtualDisplay {
  # Returns $true when Windows asks for a reboot.
  $work = Join-Path $config.work_dir 'vdd'
  if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
  Expand-Archive -LiteralPath $config.vdd.zip_path -DestinationPath $work -Force
  $inf = Get-ChildItem -LiteralPath $work -Recurse -Filter 'MttVDD.inf' | Select-Object -First 1
  if (-not $inf) { throw "MttVDD.inf not found in $($config.vdd.zip_path)." }

  # The driver reads its settings from C:\VirtualDisplayDriver.
  New-Item -ItemType Directory -Path $config.vdd.settings_dir -Force | Out-Null
  $settings = Join-Path $config.vdd.settings_dir 'vdd_settings.xml'
  $shipped = Join-Path $inf.DirectoryName 'vdd_settings.xml'
  if (-not (Test-Path -LiteralPath $settings) -and (Test-Path -LiteralPath $shipped)) {
    Copy-Item -LiteralPath $shipped -Destination $settings
  }

  # The driver is signed through SignPath Foundation, not by Microsoft, so
  # Windows asks before installing it unless the signer is a trusted
  # publisher. Nobody can answer that prompt here: trust exactly the pinned
  # signing certificate, and only for a valid signature.
  $catalog = Get-ChildItem -LiteralPath $inf.DirectoryName -Filter '*.cat' | Select-Object -First 1
  if (-not $catalog) { throw "No catalog next to $($inf.FullName)." }
  $signature = Get-AuthenticodeSignature -LiteralPath $catalog.FullName
  if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Thumbprint -ne $config.vdd.signer_thumbprint) {
    throw "Unexpected VDD signature: $($signature.Status) $($signature.SignerCertificate.Thumbprint)."
  }
  $store = New-Object Security.Cryptography.X509Certificates.X509Store('TrustedPublisher', 'LocalMachine')
  $store.Open('ReadWrite')
  try { $store.Add($signature.SignerCertificate) } finally { $store.Close() }

  if (-not ('ReGaDeviceInstaller' -as [type])) { Add-Type -TypeDefinition $deviceInstaller }
  Write-GpuLog "Installing the virtual display from $($inf.FullName)"
  return [ReGaDeviceInstaller]::Install($inf.FullName, $vddHardwareId)
}

function Disable-BasicDisplay {
  # With the virtual display working, the EC2 VGA device would only add a
  # second, primary monitor that games and Sunshine would pick by default.
  # Returns whether it disabled anything.
  $devices = @(Get-PnpDevice -Class Display -PresentOnly -ErrorAction SilentlyContinue |
    Where-Object { $_.InstanceId -like 'PCI\*' -and $_.InstanceId -notlike 'PCI\VEN_10DE*' -and $_.Status -eq 'OK' })
  foreach ($device in $devices) {
    Write-GpuLog "Disabling $($device.FriendlyName) ($($device.InstanceId))"
    Disable-PnpDevice -InstanceId $device.InstanceId -Confirm:$false
  }
  return ($devices.Count -gt 0)
}

$mutex = New-Object Threading.Mutex($false, 'Global\ReGaGpuSetup')
[void]$mutex.WaitOne()
$result = [ordered]@{
  gpu             = $false
  driver          = $false
  virtual_display = $false
  reboot_required = $false
  changed         = $false
}
try {
  $result.gpu = ((Get-NvidiaGpu).Count -gt 0)
  $result.driver = $result.gpu -and (Test-NvidiaDriver)
  $result.virtual_display = ((Get-VirtualDisplay).Count -gt 0)

  if ($Action -eq 'VirtualDisplay' -and -not $result.virtual_display) {
    $result.reboot_required = Install-VirtualDisplay
    $result.virtual_display = ((Get-VirtualDisplay).Count -gt 0)
    $result.changed = $true
  } elseif ($Action -eq 'Auto' -and $result.gpu) {
    if (-not $result.driver) {
      $state = Read-State
      $attempts = [int]$state.driver_attempts
      if ($attempts -ge 2) { throw 'The NVIDIA driver failed to install twice; see gpu-setup.log.' }
      Save-State ([pscustomobject]@{ driver_attempts = $attempts + 1 })
      Install-NvidiaDriver
      $result.driver = $true
      $result.reboot_required = $true
      $result.changed = $true
    } else {
      Set-GamingLicense
      if (-not $result.virtual_display) {
        $result.reboot_required = Install-VirtualDisplay
        $result.virtual_display = ((Get-VirtualDisplay).Count -gt 0)
        $result.changed = $true
      }
      $vdd = Get-VirtualDisplay | Where-Object { $_.Status -eq 'OK' }
      if ($config.vdd.disable_basic_display -and $vdd -and (Disable-BasicDisplay)) { $result.changed = $true }
    }
  }
  Write-GpuLog ("Result: " + (([pscustomobject]$result) | ConvertTo-Json -Compress))
} catch {
  Write-GpuLog "GPU setup failed: $_" 'error'
  throw
} finally {
  $mutex.ReleaseMutex()
}

([pscustomobject]$result) | ConvertTo-Json -Compress
if ($result.reboot_required -and -not $NoReboot) {
  & shutdown.exe /r /t 10 /d p:2:4 /c 'ReGa: finishing the GPU driver setup.'
}
