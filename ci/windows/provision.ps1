# Turns a freshly installed Windows guest into a runner for this repository.
#
# Runs once, at the first sign-in after an unattended install. Everything it
# installs is pinned by the caller through the environment, so rebuilding the
# guest a year from now produces the same toolchain rather than whatever is
# current then.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Get-Verified {
    param([string]$Url, [string]$Sha256, [string]$Path)

    Invoke-WebRequest -Uri $Url -OutFile $Path -UseBasicParsing
    if ($Sha256) {
        $actual = (Get-FileHash -Algorithm SHA256 -Path $Path).Hash
        if ($actual -ne $Sha256.ToUpper()) {
            throw "checksum mismatch for $Url : expected $Sha256, got $actual"
        }
        return
    }

    # Some vendors publish only a bootstrapper, replaced in place whenever the
    # product moves, so no checksum can be pinned against it. Its signature can:
    # an unsigned or foreign-signed download is refused just as loudly.
    $signature = Get-AuthenticodeSignature -FilePath $Path
    if ($signature.Status -ne 'Valid') {
        throw "$Url is not validly signed: $($signature.Status)"
    }
    if ($signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        throw "$Url is signed by $($signature.SignerCertificate.Subject), not Microsoft"
    }
}

# The evaluation licence runs ninety days from the image's own release, not
# from installation, and Microsoft leaves an image published far longer than
# that. An expired Windows shuts itself down every hour, which ends a test run
# mid-suite; rearming restarts the period. It is allowed a handful of times,
# which outlives any guest this rebuilds.
#
# Whether it worked is reported rather than assumed: an expired Windows shuts
# itself down on a timer, and a guest that does that mid-suite is worth knowing
# about before a lane starts blaming the tests.
$rearm = Start-Process -FilePath 'cscript.exe' `
                       -ArgumentList '//nologo', "$env:SystemRoot\System32\slmgr.vbs", '/rearm' `
                       -Wait -PassThru -NoNewWindow
if ($rearm.ExitCode -ne 0) {
    Write-Warning "could not rearm the evaluation licence (exit $($rearm.ExitCode))"
}
Write-Host '==> Licence state'
& cscript.exe //nologo "$env:SystemRoot\System32\slmgr.vbs" /dli

$settings = Get-Content 'E:\guest.json' -Raw | ConvertFrom-Json

# Everything this guest writes goes on the second disk, not the system image.
#
# A qcow2 grows to cover every block written into it and never shrinks on its
# own, so the page file alone — which Windows sizes to memory — charges the
# image as much as the guest has RAM, permanently. The host cannot shrink it in
# place, and copying it needs as much free space as the image is large, which a
# full volume by definition does not have. Held on a disk of its own the growth
# stays where it can be discarded.
#
# Absence is tolerated: a guest built before this disk existed still installs,
# it just keeps charging its system image.
$data = Get-Disk | Where-Object { $_.PartitionStyle -eq 'RAW' } | Select-Object -First 1
if ($data) {
    Write-Host '==> Preparing the data disk'
    $data | Initialize-Disk -PartitionStyle GPT -PassThru |
        New-Partition -DriveLetter D -UseMaximumSize |
        Format-Volume -FileSystem NTFS -NewFileSystemLabel 'kithara-data' -Confirm:$false |
        Out-Null

    New-Item -ItemType Directory -Force -Path 'D:\temp', 'D:\build' | Out-Null
    # Both scopes: the runner service does not inherit a user's environment.
    foreach ($scope in 'Machine', 'User') {
        [Environment]::SetEnvironmentVariable('TEMP', 'D:\temp', $scope)
        [Environment]::SetEnvironmentVariable('TMP', 'D:\temp', $scope)
    }
    $env:TEMP = 'D:\temp'
    $env:TMP = 'D:\temp'

    # The page file moves only after the automatic one is disabled; setting a
    # second one while Windows still manages its own leaves both in place, and
    # the one on C: is the one that was costing the image.
    $computer = Get-WmiObject -Class Win32_ComputerSystem -EnableAllPrivileges
    if ($computer.AutomaticManagedPagefile) {
        $computer.AutomaticManagedPagefile = $false
        $computer.Put() | Out-Null
    }
    Get-WmiObject -Class Win32_PageFileSetting | ForEach-Object { $_.Delete() }
    Set-WmiInstance -Class Win32_PageFileSetting `
        -Arguments @{ Name = 'D:\pagefile.sys'; InitialSize = 4096; MaximumSize = 16384 } |
        Out-Null
} else {
    Write-Host '==> No data disk attached; this guest writes into its system image'
}

$root = 'C:\kithara-ci'
New-Item -ItemType Directory -Force -Path $root, "$root\downloads" | Out-Null

# The Visual Studio build tools carry the MSVC linker and the Windows SDK,
# without which no Rust target on this platform links at all.
Write-Host '==> Installing the MSVC build tools'
Get-Verified -Url $settings.build_tools_url `
             -Sha256 $settings.build_tools_sha256 `
             -Path "$root\downloads\vs_buildtools.exe"
$arguments = @(
    '--quiet', '--wait', '--norestart', '--nocache',
    '--add', 'Microsoft.VisualStudio.Workload.VCTools',
    '--add', 'Microsoft.VisualStudio.Component.Windows11SDK.26100',
    '--includeRecommended'
)
$install = Start-Process -FilePath "$root\downloads\vs_buildtools.exe" `
                         -ArgumentList $arguments -Wait -PassThru
# 3010 is "installed, needs a restart", which the guest is about to do anyway.
if ($install.ExitCode -notin 0, 3010) {
    throw "the build tools installer exited with $($install.ExitCode)"
}

# A vendored native dependency builds through CMake, and the build tools carry
# one only inside their own developer prompt, where nothing here runs. This is
# the same version the Linux image pins, and for the same reason: CMake 4
# refuses any project asking for a minimum below 3.5, which several vendored
# trees still do.
Write-Host '==> Installing CMake'
Get-Verified -Url $settings.cmake_url `
             -Sha256 $settings.cmake_sha256 `
             -Path "$root\downloads\cmake.zip"
Expand-Archive -Path "$root\downloads\cmake.zip" -DestinationPath $root -Force
$cmake = (Get-ChildItem -Path $root -Directory -Filter 'cmake-*-windows-x86_64').FullName
[Environment]::SetEnvironmentVariable(
    'PATH',
    [Environment]::GetEnvironmentVariable('PATH', 'Machine') + ";$cmake\bin",
    'Machine')

# The repository's recipes are bash scripts, so `just` on this machine is
# useless without a shell to run them in. Git for Windows carries one, and the
# checkout the runner performs wants git anyway.
Write-Host '==> Installing Git for Windows'
Get-Verified -Url $settings.git_url `
             -Sha256 $settings.git_sha256 `
             -Path "$root\downloads\git.exe"
$install = Start-Process -FilePath "$root\downloads\git.exe" `
                         -ArgumentList '/VERYSILENT', '/NORESTART', '/NOCANCEL', `
                                       '/SP-', '/SUPPRESSMSGBOXES' `
                         -Wait -PassThru
if ($install.ExitCode -ne 0) {
    throw "the Git installer exited with $($install.ExitCode)"
}
[Environment]::SetEnvironmentVariable(
    'PATH',
    [Environment]::GetEnvironmentVariable('PATH', 'Machine') + ';C:\Program Files\Git\bin',
    'Machine')

Write-Host '==> Installing the Rust toolchain'
Get-Verified -Url $settings.rustup_url `
             -Sha256 $settings.rustup_sha256 `
             -Path "$root\downloads\rustup-init.exe"
& "$root\downloads\rustup-init.exe" `
    -y --no-modify-path --profile minimal `
    --default-toolchain $settings.stable_toolchain
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
[Environment]::SetEnvironmentVariable(
    'PATH',
    "$env:USERPROFILE\.cargo\bin;" + [Environment]::GetEnvironmentVariable('PATH', 'Machine'),
    'Machine')

foreach ($tool in $settings.cargo_tools.PSObject.Properties) {
    Write-Host "==> Installing $($tool.Name) $($tool.Value)"
    cargo install --locked --version $tool.Value $tool.Name
    if ($LASTEXITCODE -ne 0) { throw "cargo install $($tool.Name) failed" }
}

Write-Host '==> Installing the GitHub Actions runner'
New-Item -ItemType Directory -Force -Path "$root\runner" | Out-Null
Get-Verified -Url $settings.runner_url `
             -Sha256 $settings.runner_sha256 `
             -Path "$root\downloads\runner.zip"
Expand-Archive -Path "$root\downloads\runner.zip" -DestinationPath "$root\runner" -Force

# What the guest does on every sign-in from here on. It registers once, with
# credentials the host leaves on the answer volume, and then serves jobs until
# it is restarted. The registration outlives a restart, so the enrolment branch
# is taken exactly once per installed guest; a guest that boots before the host
# has left it anything says so and stops rather than looking busy.
$runner = @'
Set-Location C:\kithara-ci\runner
if (-not (Test-Path '.runner')) {
    if (-not (Test-Path 'E:\enrolment.json')) {
        Write-Host 'no enrolment on E:; nothing to register with'
        exit 1
    }
    $enrolment = Get-Content 'E:\enrolment.json' -Raw | ConvertFrom-Json
    .\config.cmd --unattended --replace --work _work `
                 --url $enrolment.url --token $enrolment.token `
                 --name $enrolment.name --labels $enrolment.labels
    if ($LASTEXITCODE -ne 0) { throw "runner enrolment failed with $LASTEXITCODE" }
}
.\run.cmd
'@
Set-Content -Path "$root\runner\start.ps1" -Value $runner -Encoding UTF8

# Windows runs whatever is in this folder at sign-in, which needs no scheduled
# task and no password to register one with.
$startup = [Environment]::GetFolderPath('Startup')
Set-Content -Path "$startup\kithara-ci-runner.cmd" `
            -Value "powershell -NoProfile -ExecutionPolicy Bypass -File $root\runner\start.ps1" `
            -Encoding ASCII

Remove-Item -Recurse -Force "$root\downloads"
Write-Host '==> Guest provisioned'

# The sign-in that ran this one was granted by the answer file; every later one
# is the automatic sign-in, which only takes effect on a restart.
Restart-Computer -Force
