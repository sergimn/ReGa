# Unit tests for the agent's pure functions. They run under PowerShell 7 on
# Linux and under Windows PowerShell 5.1 on a Windows runner (the version
# the machine uses). Run: Invoke-Pester infra/ansible/tests/pester

BeforeAll {
  $module = Join-Path $PSScriptRoot '../../roles/library_agent/files/ReGa.Agent.psm1'
  Import-Module $module -Force
}

Describe 'Get-SunshineClientCount' {
  It 'counts no clients in an empty log' {
    Get-SunshineClientCount @() | Should -Be 0
  }

  It 'counts a connected client' {
    $log = @(
      '[2026-10-11 10:00:00.000]: Info: Sunshine version: v2026.914.233613 commit: abc',
      '[2026-10-11 10:01:00.000]: Info: CLIENT CONNECTED'
    )
    Get-SunshineClientCount $log | Should -Be 1
  }

  It 'counts a client that left as gone' {
    $log = @(
      '[2026-10-11 10:01:00.000]: Info: CLIENT CONNECTED',
      '[2026-10-11 10:30:00.000]: Info: CLIENT DISCONNECTED'
    )
    Get-SunshineClientCount $log | Should -Be 0
  }

  It 'tracks overlapping clients' {
    $log = @(
      '[x]: Info: CLIENT CONNECTED',
      '[x]: Info: CLIENT CONNECTED',
      '[x]: Info: CLIENT DISCONNECTED'
    )
    Get-SunshineClientCount $log | Should -Be 1
  }

  It 'forgets sessions from before the last start' {
    $log = @(
      '[x]: Info: Sunshine version: v1 commit: a',
      '[x]: Info: CLIENT CONNECTED',
      '[x]: Info: Sunshine version: v1 commit: a'
    )
    Get-SunshineClientCount $log | Should -Be 0
  }

  It 'counts a ping timeout as a disconnect' {
    $log = @(
      '[x]: Info: CLIENT CONNECTED',
      '[x]: Info: 203.0.113.7: Ping Timeout'
    )
    Get-SunshineClientCount $log | Should -Be 0
  }

  It 'ignores an initial ping timeout' {
    Get-SunshineClientCount @('[x]: Info: CLIENT CONNECTED', '[x]: Error: Initial Ping Timeout') | Should -Be 1
  }

  It 'never goes below zero' {
    Get-SunshineClientCount @('[x]: Info: CLIENT DISCONNECTED', '[x]: Info: CLIENT CONNECTED') | Should -Be 1
  }
}

Describe 'Get-IdleDecision' {
  BeforeAll {
    $boot = [datetime]::new(2026, 10, 11, 10, 0, 0, [DateTimeKind]::Utc)
  }

  It 'starts the idle clock at boot' {
    $d = Get-IdleDecision -State $null -Now $boot.AddMinutes(10) -BootTime $boot -Active $false -IdleMinutes 15 -IdleStopEnabled $true
    $d.IdleMinutes | Should -Be 10
    $d.ShouldStop | Should -BeFalse
  }

  It 'stops after the idle time with nobody active' {
    $d = Get-IdleDecision -State $null -Now $boot.AddMinutes(15) -BootTime $boot -Active $false -IdleMinutes 15 -IdleStopEnabled $true
    $d.ShouldStop | Should -BeTrue
  }

  It 'never stops while active' {
    $state = [pscustomobject]@{ last_active_utc = $boot.AddMinutes(1).ToString('o') }
    $d = Get-IdleDecision -State $state -Now $boot.AddMinutes(90) -BootTime $boot -Active $true -IdleMinutes 15 -IdleStopEnabled $true
    $d.ShouldStop | Should -BeFalse
    $d.LastActiveUtc | Should -Be $boot.AddMinutes(90)
  }

  It 'counts from the last activity' {
    $state = [pscustomobject]@{ last_active_utc = $boot.AddMinutes(30).ToString('o') }
    $d = Get-IdleDecision -State $state -Now $boot.AddMinutes(44) -BootTime $boot -Active $false -IdleMinutes 15 -IdleStopEnabled $true
    $d.ShouldStop | Should -BeFalse
    $d.IdleMinutes | Should -Be 14
  }

  It 'ignores activity from before this boot' {
    $state = [pscustomobject]@{ last_active_utc = $boot.AddDays(-2).ToString('o') }
    $d = Get-IdleDecision -State $state -Now $boot.AddMinutes(5) -BootTime $boot -Active $false -IdleMinutes 15 -IdleStopEnabled $true
    $d.ShouldStop | Should -BeFalse
  }

  It 'accepts dates PowerShell 7 already parsed' {
    $state = [pscustomobject]@{ last_active_utc = $boot.AddMinutes(30) }
    $d = Get-IdleDecision -State $state -Now $boot.AddMinutes(50) -BootTime $boot -Active $false -IdleMinutes 15 -IdleStopEnabled $true
    $d.ShouldStop | Should -BeTrue
  }

  It 'does nothing when idle stop is off' {
    $d = Get-IdleDecision -State $null -Now $boot.AddHours(5) -BootTime $boot -Active $false -IdleMinutes 15 -IdleStopEnabled $false
    $d.ShouldStop | Should -BeFalse
  }
}

Describe 'Test-SameBoot' {
  It 'matches a recorded boot time' {
    $boot = [datetime]::new(2026, 10, 11, 10, 0, 0, [DateTimeKind]::Utc)
    Test-SameBoot $boot.AddSeconds(1).ToString('o') $boot | Should -BeTrue
  }

  It 'rejects another boot or nothing' {
    $boot = [datetime]::new(2026, 10, 11, 10, 0, 0, [DateTimeKind]::Utc)
    Test-SameBoot $boot.AddHours(-3).ToString('o') $boot | Should -BeFalse
    Test-SameBoot $null $boot | Should -BeFalse
  }
}

Describe 'Resolve-LibraryTarget' {
  It 'derives the bucket from the account' {
    $t = Resolve-LibraryTarget -Config ([pscustomobject]@{ prefix = 'steam-library' }) `
      -Identity ([pscustomobject]@{ accountId = '123456789012'; region = 'eu-central-1' })
    $t.Bucket | Should -Be 'rega-games-123456789012'
    $t.Region | Should -Be 'eu-central-1'
    $t.Uri | Should -Be 's3://rega-games-123456789012/steam-library/'
  }

  It 'prefers the configured bucket and region' {
    $t = Resolve-LibraryTarget -Config ([pscustomobject]@{ bucket = 'b'; bucket_region = 'eu-north-1'; prefix = '/lib/' }) `
      -Identity ([pscustomobject]@{ accountId = '1'; region = 'eu-central-1' })
    $t.Uri | Should -Be 's3://b/lib/'
    $t.Region | Should -Be 'eu-north-1'
  }

  It 'fails off EC2 without a bucket' {
    { Resolve-LibraryTarget -Config ([pscustomobject]@{ prefix = 'p' }) -Identity $null } | Should -Throw
  }
}

Describe 'Get-SyncArguments' {
  It 'builds a restore without --delete' {
    $a = Get-SyncArguments -Source 's3://b/p/' -Destination 'D:\SteamLibrary' -Region 'eu-north-1' -Excludes @('steamapps/downloading/*')
    ($a -join ' ') | Should -Be 's3 sync s3://b/p/ D:\SteamLibrary --region eu-north-1 --no-progress --only-show-errors --exclude steamapps/downloading/*'
  }

  It 'mirrors deletes on upload' {
    $a = Get-SyncArguments -Source 'D:\SteamLibrary' -Destination 's3://b/p/' -Region 'r' -Excludes @() -Delete
    $a[-1] | Should -Be '--delete'
  }

  It 'returns an array even with one exclude' {
    $a = Get-SyncArguments -Source 'a' -Destination 'b' -Region 'r' -Excludes @('x')
    $a.GetType().IsArray | Should -BeTrue
  }
}

Describe 'Get-RestoredMarkerPath' {
  It 'puts the marker at the root of the library disk' {
    Get-RestoredMarkerPath 'D:\SteamLibrary' | Should -Be 'D:\.rega-restored.json'
  } -Skip:(-not $IsWindows -and $PSVersionTable.PSEdition -eq 'Core')
}

Describe 'Steam library folders' {
  It 'creates libraryfolders.vdf with the Steam folder and the library' {
    $text = Add-SteamLibraryFolder -Text '' -SteamPath 'C:\Program Files (x86)\Steam' -LibraryPath 'D:\SteamLibrary' -ContentId '42'
    $text | Should -Match '"0"\s*\{\s*"path"\s+"C:\\\\Program Files \(x86\)\\\\Steam"'
    $text | Should -Match '"1"\s*\{\s*"path"\s+"D:\\\\SteamLibrary"'
    $text | Should -Match '"contentid"\s+"42"'
    $text.Trim().EndsWith('}') | Should -BeTrue
  }

  It 'adds the library after the existing entries' {
    $existing = "`"libraryfolders`"`n{`n`t`"0`"`n`t{`n`t`t`"path`"`t`t`"C:\\Program Files (x86)\\Steam`"`n`t`t`"apps`"`n`t`t{`n`t`t`t`"228980`"`t`t`"0`"`n`t`t}`n`t}`n}`n"
    $text = Add-SteamLibraryFolder -Text $existing -SteamPath 'C:\Program Files (x86)\Steam' -LibraryPath 'D:\SteamLibrary' -ContentId '7'
    $text | Should -Match '"1"\s*\{\s*"path"\s+"D:\\\\SteamLibrary"'
    $text | Should -Match '"228980"'
    ([regex]::Matches($text, '\{')).Count | Should -Be ([regex]::Matches($text, '\}')).Count
  }

  It 'leaves a file that already lists the library alone' {
    $existing = Add-SteamLibraryFolder -Text '' -SteamPath 'C:\Steam' -LibraryPath 'D:\SteamLibrary' -ContentId '1'
    Add-SteamLibraryFolder -Text $existing -SteamPath 'C:\Steam' -LibraryPath 'd:\steamlibrary\' -ContentId '1' | Should -BeNullOrEmpty
  }

  It 'reads the content id of a library' {
    Get-VdfContentId (New-LibraryFolderVdf '123') | Should -Be '123'
  }

  It 'makes positive 63-bit content ids' {
    $id = New-SteamContentId
    $id | Should -Match '^\d+$'
    [decimal]$id | Should -BeLessOrEqual ([decimal][long]::MaxValue)
  }
}

Describe 'Steam apps' {
  It 'reads values from an app manifest' {
    $acf = "`"AppState`"`n{`n`t`"appid`"`t`t`"228980`"`n`t`"installdir`"`t`t`"Steamworks Shared`"`n}`n"
    Get-AcfValue $acf 'appid' | Should -Be '228980'
    Get-AcfValue $acf 'installdir' | Should -Be 'Steamworks Shared'
    Get-AcfValue $acf 'missing' | Should -BeNullOrEmpty
  }

  It 'accepts only plain folder names' {
    Test-SafeFolderName 'Half-Life 2' | Should -BeTrue
    foreach ($bad in @('', ' ', '.', '..', 'a/b', 'a\b', '..\x', 'C:x')) {
      Test-SafeFolderName $bad | Should -BeFalse -Because "'$bad' is not a single folder"
    }
  }

  It 'lists installed apps and skips unsafe folders' {
    $apps = Join-Path $TestDrive 'steamapps'
    New-Item -ItemType Directory -Path $apps -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $apps 'appmanifest_10.acf') -Value "`"AppState`"`n{`n`t`"appid`"`t`t`"10`"`n`t`"installdir`"`t`t`"Counter-Strike`"`n}"
    Set-Content -LiteralPath (Join-Path $apps 'appmanifest_20.acf') -Value "`"AppState`"`n{`n`t`"appid`"`t`t`"20`"`n`t`"installdir`"`t`t`"..`"`n}"
    $found = Get-SteamApps $apps
    $found.Count | Should -Be 1
    $found['10'] | Should -Be 'Counter-Strike'
  }

  It 'returns no apps for a missing folder' {
    (Get-SteamApps (Join-Path $TestDrive 'nowhere')).Count | Should -Be 0
  }

  It 'reads the app table back from JSON' {
    $json = '{"10":"Counter-Strike","20":"Portal"}' | ConvertFrom-Json
    $table = ConvertTo-AppTable $json
    $table['20'] | Should -Be 'Portal'
    (ConvertTo-AppTable $null).Count | Should -Be 0
  }

  It 'finds uninstalled apps and keeps folders still in use' {
    $known = @{ '10' = 'Counter-Strike'; '20' = 'Shared'; '30' = 'Portal' }
    $current = @{ '10' = 'Counter-Strike'; '21' = 'shared' }
    $removed = Get-RemovedApps -Known $known -Current $current -Present @('10', '21')
    $removed.Count | Should -Be 2
    ($removed | Where-Object AppId -eq '20').DeleteFolder | Should -BeFalse
    ($removed | Where-Object AppId -eq '30').DeleteFolder | Should -BeTrue
  }

  It 'does not count a manifest that fails to parse as removed' {
    $known = @{ '10' = 'Counter-Strike' }
    $removed = Get-RemovedApps -Known $known -Current @{} -Present @('10')
    $removed.Count | Should -Be 0
  }

  It 'removes nothing when nothing was known' {
    $removed = Get-RemovedApps -Known @{} -Current @{ '10' = 'Counter-Strike' } -Present @('10')
    $removed.Count | Should -Be 0
  }

  It 'stamps manifests whether they parse or not' {
    $apps = Join-Path $TestDrive 'stamps'
    New-Item -ItemType Directory -Path $apps -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $apps 'appmanifest_7.acf') -Value 'half a file'
    $stamps = Get-ManifestStamps $apps
    $stamps.Keys | Should -Be @('7')
    $stamps['7'] | Should -Match '^\d+:\d+$'
  }

  It 'never overwrites a build another machine uploaded' {
    Test-UploadAllowed -S3Build '' -BaseBuild '' -LocalBuild '5' | Should -BeTrue
    Test-UploadAllowed -S3Build '4' -BaseBuild '4' -LocalBuild '5' | Should -BeTrue
    Test-UploadAllowed -S3Build '5' -BaseBuild '4' -LocalBuild '5' | Should -BeTrue
    Test-UploadAllowed -S3Build '6' -BaseBuild '4' -LocalBuild '4' | Should -BeFalse
    Test-UploadAllowed -S3Build '6' -BaseBuild '' -LocalBuild '5' | Should -BeFalse
  }
}

Describe 'Write-JsonFile' {
  It 'writes the file and leaves no temporary file behind' {
    $path = Join-Path $TestDrive 'state.json'
    Write-JsonFile $path ([pscustomobject]@{ a = 1 })
    (Get-Content -LiteralPath $path -Raw | ConvertFrom-Json).a | Should -Be 1
    @(Get-ChildItem -LiteralPath $TestDrive -Filter '*.tmp').Count | Should -Be 0
  }
}

Describe 'Invoke-LibraryUpload' {
  BeforeAll {
    function New-Manifest([string]$Path, [string]$Id, [string]$Dir, [string]$Build) {
      Set-Content -LiteralPath $Path -Value "`"AppState`"`n{`n`t`"appid`"`t`t`"$Id`"`n`t`"installdir`"`t`t`"$Dir`"`n`t`"buildid`"`t`t`"$Build`"`n}"
    }
  }

  BeforeEach {
    $script:library = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
    $script:apps = Join-Path $library 'steamapps'
    $script:marker = Join-Path $library 'marker.json'
    $script:target = [pscustomobject]@{ Bucket = 'b'; Region = 'eu-north-1'; Uri = 's3://b/steam-library/' }
    New-Item -ItemType Directory -Path (Join-Path $apps 'common/Portal') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $apps 'common/Half-Life') -Force | Out-Null
    New-Manifest (Join-Path $apps 'appmanifest_400.acf') '400' 'Portal' '100'
    New-Manifest (Join-Path $apps 'appmanifest_70.acf') '70' 'Half-Life' '7'
    # As the restore records it: Portal, Half-Life and Portal 2.
    $snapshot = Get-LibrarySnapshot $apps
    $snapshot.apps['620'] = 'Portal 2'
    $snapshot.stamps['620'] = '1:1'
    $snapshot.builds['620'] = '200'
    Write-JsonFile $marker ([pscustomobject]@{ restored_utc = '2026-10-11T10:00:00Z'; apps = $snapshot.apps; stamps = $snapshot.stamps; builds = $snapshot.builds })
    # Then Portal updates to build 101 and Portal 2 is uninstalled.
    Start-Sleep -Milliseconds 20
    New-Manifest (Join-Path $apps 'appmanifest_400.acf') '400' 'Portal' '101'
    $script:calls = [Collections.ArrayList]::new()
    $script:s3Build = '100'
    Mock Invoke-Aws -ModuleName ReGa.Agent { [void]$script:calls.Add($Arguments -join ' '); return 0 }
    # S3 holds the builds this disk was restored from, except Portal's
    # (set per test).
    Mock Get-S3Text -ModuleName ReGa.Agent {
      $build = switch -Wildcard ($Uri) { '*appmanifest_400.acf' { $script:s3Build } '*appmanifest_70.acf' { '7' } '*appmanifest_620.acf' { '200' } default { '' } }
      [pscustomobject]@{ Ok = $true; Found = ($build -ne ''); Text = "`"buildid`"`t`t`"$build`"" }
    }
  }

  It 'never deletes across the whole library' {
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @('steamapps/downloading/*')).Ok | Should -BeTrue
    $root = @($calls | Where-Object { $_ -like "s3 sync $library s3://b/steam-library/ *" })
    $root.Count | Should -Be 1
    $root[0] | Should -Not -Match '--delete'
    foreach ($pattern in @('steamapps/common/\*', 'steamapps/appmanifest_\*', 'steamapps/workshop/\*', 'steamapps/downloading/\*')) {
      $root[0] | Should -Match "--exclude $pattern"
    }
  }

  It 'uploads only the changed game, its files before its manifest' {
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeTrue
    $folder = [array]::FindIndex([string[]]$calls, [Predicate[string]]{ param($c) $c -match 'steamapps/common/Portal/ .*--delete$' })
    $manifest = [array]::FindIndex([string[]]$calls, [Predicate[string]]{ param($c) $c -match '^s3 cp .*appmanifest_400\.acf s3://b/steam-library/steamapps/appmanifest_400\.acf' })
    $folder | Should -BeGreaterOrEqual 0
    $manifest | Should -BeGreaterThan $folder
    @($calls | Where-Object { $_ -match 'Half-Life|appmanifest_70' }).Count | Should -Be 0
  }

  It 'deletes the uninstalled game, manifest first' {
    Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @() | Out-Null
    $removals = @($calls | Where-Object { $_ -match '^s3 rm .*(620|Portal 2)' })
    $removals[0] | Should -Be 's3 rm s3://b/steam-library/steamapps/appmanifest_620.acf --region eu-north-1 --only-show-errors'
    $calls | Should -Contain 's3 rm s3://b/steam-library/steamapps/common/Portal 2/ --recursive --region eu-north-1 --only-show-errors'
  }

  It 'records what it uploaded so the next run does nothing' {
    Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @() | Out-Null
    $saved = Read-JsonFile $marker
    (ConvertTo-AppTable $saved.apps).Keys | Sort-Object | Should -Be @('400', '70')
    (ConvertTo-AppTable $saved.builds)['400'] | Should -Be '101'
    $saved.restored_utc | Should -Not -BeNullOrEmpty
    $calls.Clear()
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeTrue
    @($calls | Where-Object { $_ -notlike "s3 sync $library *" }).Count | Should -Be 0
  }

  It 'skips a game another machine uploaded a different build of' {
    $script:s3Build = '102'
    $result = Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()
    $result.Ok | Should -BeTrue
    $result.Skipped | Should -Be @('400')
    @($calls | Where-Object { $_ -match 'Portal/|appmanifest_400' }).Count | Should -Be 0
  }

  It 'keeps the manifest back and retries when the folder upload fails' {
    Mock Invoke-Aws -ModuleName ReGa.Agent { [void]$script:calls.Add($Arguments -join ' '); if ($Arguments -contains '--delete') { return 1 } ; return 0 }
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeFalse
    @($calls | Where-Object { $_ -match '^s3 cp .*appmanifest_400' }).Count | Should -Be 0
    (ConvertTo-AppTable (Read-JsonFile $marker).builds)['400'] | Should -Be '100'
  }

  It 'keeps a removal to retry when it fails' {
    Mock Invoke-Aws -ModuleName ReGa.Agent { if ($Arguments[1] -eq 'rm') { return 1 } ; return 0 }
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeFalse
    (ConvertTo-AppTable (Read-JsonFile $marker).apps).ContainsKey('620') | Should -BeTrue
  }

  It 'uploads nothing from a disk that was not restored' {
    Remove-Item -LiteralPath $marker
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeFalse
    $calls.Count | Should -Be 0
  }

  It 'uploads every game before a stop, changed or not' {
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @() -All).Ok | Should -BeTrue
    @($calls | Where-Object { $_ -match 'steamapps/common/Half-Life/ .*--delete$' }).Count | Should -Be 1
    @($calls | Where-Object { $_ -match '^s3 cp .*appmanifest_70\.acf' }).Count | Should -Be 1
  }

  It 'sees a workshop change as a change' {
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeTrue
    New-Item -ItemType Directory -Path (Join-Path $apps 'workshop/content/70/123') -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $apps 'workshop/appworkshop_70.acf') -Value 'items'
    $calls.Clear()
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeTrue
    @($calls | Where-Object { $_ -match 'steamapps/workshop/content/70/ .*--delete$' }).Count | Should -Be 1
    @($calls | Where-Object { $_ -match '^s3 cp .*appworkshop_70\.acf' }).Count | Should -Be 1
  }

  It 'leaves an uninstalled game alone when another machine uploaded a new build' {
    Mock Get-S3Text -ModuleName ReGa.Agent {
      $build = if ($Uri -like '*appmanifest_620.acf') { '201' } else { $script:s3Build }
      [pscustomobject]@{ Ok = $true; Found = $true; Text = "`"buildid`"`t`t`"$build`"" }
    }
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeTrue
    @($calls | Where-Object { $_ -match '^s3 rm .*(620|Portal 2)' }).Count | Should -Be 0
    (ConvertTo-AppTable (Read-JsonFile $marker).apps).ContainsKey('620') | Should -BeFalse
  }

  It 'cleans up a partial upload of a new game that is uninstalled' {
    New-Item -ItemType Directory -Path (Join-Path $apps 'common/New Game') -Force | Out-Null
    New-Manifest (Join-Path $apps 'appmanifest_900.acf') '900' 'New Game' '1'
    Mock Get-S3Text -ModuleName ReGa.Agent { [pscustomobject]@{ Ok = $true; Found = $false; Text = '' } }
    Mock Invoke-Aws -ModuleName ReGa.Agent { [void]$script:calls.Add($Arguments -join ' '); if ($Arguments -contains '--delete') { return 1 } ; return 0 }
    (Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @()).Ok | Should -BeFalse
    Remove-Item -LiteralPath (Join-Path $apps 'appmanifest_900.acf')
    $calls.Clear()
    Mock Invoke-Aws -ModuleName ReGa.Agent { [void]$script:calls.Add($Arguments -join ' '); return 0 }
    Invoke-LibraryUpload -AwsPath 'aws' -LibraryPath $library -MarkerPath $marker -Target $target -Excludes @() | Out-Null
    $calls | Should -Contain 's3 rm s3://b/steam-library/steamapps/common/New Game/ --recursive --region eu-north-1 --only-show-errors'
  }
}

Describe 'Get-S3Text' {
  BeforeAll {
    # A stand-in for the AWS CLI: prints its output and exits with a code.
    $script:stub = Join-Path $TestDrive 'aws-stub.ps1'
    Set-Content -LiteralPath $stub -Value @'
param([Parameter(ValueFromRemainingArguments = $true)] $Rest)
if ($env:REGA_STUB_ERROR) { Write-Error $env:REGA_STUB_ERROR; exit 1 }
'"buildid"		"42"'
exit 0
'@
  }
  AfterEach { $env:REGA_STUB_ERROR = $null }

  It 'returns the object text' {
    $r = Get-S3Text -AwsPath $stub -Uri 's3://b/k' -Region 'r'
    $r.Ok | Should -BeTrue
    $r.Found | Should -BeTrue
    Get-AcfValue $r.Text 'buildid' | Should -Be '42'
  }

  It 'reports a missing object as not found' {
    $env:REGA_STUB_ERROR = 'download failed: s3://b/k to - An error occurred (404) when calling the HeadObject operation: Not Found'
    $r = Get-S3Text -AwsPath $stub -Uri 's3://b/k' -Region 'r'
    $r.Ok | Should -BeTrue
    $r.Found | Should -BeFalse
  }

  It 'reports other errors as failures' {
    $env:REGA_STUB_ERROR = 'An error occurred (403) when calling the HeadObject operation: Forbidden'
    $r = Get-S3Text -AwsPath $stub -Uri 's3://b/k' -Region 'r'
    $r.Ok | Should -BeFalse
  }
}
