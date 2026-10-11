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
