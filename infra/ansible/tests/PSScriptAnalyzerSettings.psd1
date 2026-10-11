@{
  Severity     = @('Error', 'Warning')
  ExcludeRules = @(
    # The scripts log to files and print JSON for Ansible; Write-Host is not used.
    'PSAvoidUsingWriteHost',
    # Scheduled-task and Ansible scripts, not interactive cmdlets.
    'PSUseShouldProcessForStateChangingFunctions',
    # Agent functions return lists by design (Get-Signals, Get-SyncArguments).
    'PSUseSingularNouns',
    # The Sunshine and WireGuard scripts take a password and keys from
    # Ansible (no_log) and pass them to native tools as plain strings.
    'PSAvoidUsingPlainTextForPassword',
    'PSAvoidUsingUsernameAndPasswordParams'
  )
}
