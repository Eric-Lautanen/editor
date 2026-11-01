# Inspect a built MSI by querying its Windows Installer database directly
# (late-bound COM; no msidb.exe comes with WiX v4+ and the runner image only
# carries legacy WiX v3 tooling this replaces).
#
# The `.wxs` text is checked by tests in `src/core/file_kind.rs` and
# `src/update.rs`, but a text check cannot see what WiX actually linked:
# a component defined but never referenced, a dialog the UI sequence does
# not show, a Property WiX silently dropped. Those are what this reads.
#
# Usage: pwsh tools/check-msi.ps1 -Path <msi>
# Asserts, in the built package: the payload, the Add/Remove Programs
# properties, and both of the package's own dialog tables.
param(
    [Parameter(Mandatory)]
    [string]$Path
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $Path)) { throw "no MSI at $Path" }

# Open mode 0 is read-only (the few open modes the API names are constants,
# not enums).
$installer = [System.Activator]::CreateInstance(
    [System.Type]::GetTypeFromProgID('WindowsInstaller.Installer'))
$database = $installer.GetType().InvokeMember(
    'OpenDatabase', 'InvokeMethod', $null, $installer,
    @((Resolve-Path $Path).Path, 0))

function Get-MsiRows {
    param($Database, [string]$Sql, [int]$Columns)
    $view = $database.GetType().InvokeMember(
        'OpenView', 'InvokeMethod', $null, $Database, @($Sql))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, @()) | Out-Null
    $rows = [System.Collections.Generic.List[object]]::new()
    while ($true) {
        # StringData(1) is null past the last row; Fetch returning null ends
        # the loop, and StringData fetches *this row's* column i.
        $record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, @())
        if ($null -eq $record) { break }
        $rows.Add(@(
            for ($i = 1; $i -le $Columns; $i++) {
                $record.GetType().InvokeMember(
                    'StringData', 'GetProperty', $null, $record, @($i))
            }))
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, @()) | Out-Null
    return $rows
}

# ---------------------------------------------------------------- payload
# File names land in the table as long name | short name for long names;
# both sides must be the payload, so take everything after the pipe.
$files = (Get-MsiRows $database 'SELECT `FileName` FROM `File`' 1 |
    ForEach-Object { ($_ -split '\|') | Select-Object -Last 1 })
if ($files -notcontains 'editor.exe') {
    throw ("File table has no editor.exe. Payload files: $($files -join ', ')")
}
Write-Host "payload OK: $($files.Count) file(s), editor.exe present"

# ------------------------------------------------------- ARP properties
$rows = Get-MsiRows $database 'SELECT `Property`, `Value` FROM `Property`' 2
$props = @{}
foreach ($row in $rows) { $props[$row[0]] = $row[1] }
$arpChecks = @(
    @{ Key = 'ProductName';     Want = 'Editor' },
    @{ Key = 'ProductVersion';  Want = '0.0.0' }, # built unsigned for CI; release.yml stamps the tag
    @{ Key = 'Manufacturer';    Want = 'Eric-Lautanen' },
    @{ Key = 'UpgradeCode' },
    @{ Key = 'ARPPRODUCTICON';  Want = 'editor.ico' },
    @{ Key = 'ARPCOMMENTS' }
)
foreach ($check in $arpChecks) {
    $got = $props[$check.Key]
    if (-not $got) { throw "Property table has no $($check.Key)" }
    if ($check.Want -and $got -ne $check.Want) {
        throw "$($check.Key) is '$got', expected '$($check.Want)'"
    }
}
Write-Host ("ARP OK: ProductName={0}, UpgradeCode and icon/comments set" -f $props['ProductName'])

# ---------------------------------------------------------------- dialogs
$dialogs = Get-MsiRows $database 'SELECT `Dialog` FROM `Dialog`' 1 |
    ForEach-Object { $_ }
foreach ($want in @('EditorWelcomeDlg', 'EditorDoneDlg', 'ProgressDlg')) {
    if ($dialogs -notcontains $want) {
        throw ("Dialog table lacks {0}. Dialogs present: {1}" -f
            $want, ($dialogs -join ', '))
    }
}
Write-Host "dialogs OK: both own pages plus the progress page are linked"

Write-Host "MSI OK: $Path"
