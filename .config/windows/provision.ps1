# Turns a freshly installed Windows guest into a runner for this repository.
#
# Runs once, at the first sign-in after an unattended install. Everything it
# installs is pinned by the caller through E:\guest.json, so rebuilding the
# guest a year from now produces the same toolchain rather than whatever is
# current then.
#
# The host sees nothing else of a guest that is not yet a runner, so each step
# is announced on the first serial port, which the host keeps as a file, and so
# is how the script ended: `done`, or `failed:` with the error that stopped it.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# One line to the host. The port is opened per line rather than held, because
# the runner's start script writes to it too.
function Send-Host {
    param([string]$Line)

    $port = New-Object System.IO.Ports.SerialPort 'COM1', 115200
    $port.Open()
    try {
        $port.WriteLine($Line)
    } finally {
        $port.Close()
    }
}

function Start-Step {
    param([string]$Name)

    Write-Host "==> $Name"
    Send-Host "kithara-guest: step $Name"
}

# Whatever stops this script reaches the host as its last word.
trap {
    Send-Host ("kithara-guest: failed: $_" -replace '\s+', ' ')
    break
}

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
Start-Step 'Rearming the evaluation licence'
$rearm = Start-Process -FilePath 'cscript.exe' `
                       -ArgumentList '//nologo', "$env:SystemRoot\System32\slmgr.vbs", '/rearm' `
                       -Wait -PassThru -NoNewWindow
if ($rearm.ExitCode -ne 0) {
    Write-Warning "could not rearm the evaluation licence (exit $($rearm.ExitCode))"
}
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
    Start-Step 'Preparing the data disk'
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
Start-Step 'Installing the MSVC build tools'
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
Start-Step 'Installing CMake'
Get-Verified -Url $settings.cmake_url `
             -Sha256 $settings.cmake_sha256 `
             -Path "$root\downloads\cmake.zip"
Expand-Archive -Path "$root\downloads\cmake.zip" -DestinationPath $root -Force
$cmake = (Get-ChildItem -Path $root -Directory -Filter 'cmake-*-windows-x86_64').FullName
[Environment]::SetEnvironmentVariable(
    'PATH',
    [Environment]::GetEnvironmentVariable('PATH', 'Machine') + ";$cmake\bin",
    'Machine')

# The encoder links Monkey's Audio as a static library, which only the Visual
# Studio project its authors ship builds on Windows: their CMake build makes a
# DLL here. Whole-program optimisation stays off, because it leaves objects
# that only the same compiler can read, not the linker Rust drives. The C
# runtime becomes the DLL one Rust links by default; the static one the
# project asks for would be a second C runtime in the same binary.
Start-Step "Building Monkey's Audio"
Get-Verified -Url $settings.monkeys_audio_source_url `
             -Sha256 $settings.monkeys_audio_source_sha256 `
             -Path "$root\downloads\monkeys-audio.zip"
$monkeySource = "$root\monkeys-audio-source"
$monkeyPrefix = "$root\monkeys-audio"
Expand-Archive -Path "$root\downloads\monkeys-audio.zip" -DestinationPath $monkeySource -Force
$msbuild = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" `
    -latest -products * -requires Microsoft.Component.MSBuild `
    -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
if (-not $msbuild) { throw 'the build tools carry no MSBuild' }
$runtime = "$root\downloads\dynamic-runtime.props"
Set-Content -Path $runtime -Encoding UTF8 -Value @'
<Project>
  <ItemDefinitionGroup>
    <ClCompile>
      <RuntimeLibrary>MultiThreadedDLL</RuntimeLibrary>
    </ClCompile>
  </ItemDefinitionGroup>
</Project>
'@
$project = "$monkeySource\Source\Projects\Visual Studio - 2022\MACLib"
& $msbuild "$project\MACLib.vcxproj" `
    /p:Configuration=Release /p:Platform=x64 /p:WholeProgramOptimization=false `
    "/p:ForceImportBeforeCppTargets=$runtime" /m:2 /nologo
if ($LASTEXITCODE -ne 0) { throw "Monkey's Audio build failed with $LASTEXITCODE" }
New-Item -ItemType Directory -Force -Path "$monkeyPrefix\lib" | Out-Null
Copy-Item -Path "$project\x64\Release\MACLib.lib" -Destination "$monkeyPrefix\lib\MAC.lib"
[Environment]::SetEnvironmentVariable('MONKEYS_AUDIO_DIR', $monkeyPrefix, 'Machine')

# `ffmpeg-next` builds against the FFmpeg that FFMPEG_DIR names, headers and
# import libraries, and its tests load the DLLs beside them from PATH.
Start-Step 'Installing FFmpeg'
Get-Verified -Url $settings.ffmpeg_url `
             -Sha256 $settings.ffmpeg_sha256 `
             -Path "$root\downloads\ffmpeg.zip"
Expand-Archive -Path "$root\downloads\ffmpeg.zip" -DestinationPath $root -Force
$ffmpeg = (Get-ChildItem -Path $root -Directory -Filter 'ffmpeg-*-shared-*').FullName
if (-not $ffmpeg) { throw 'the FFmpeg archive holds no shared build' }
[Environment]::SetEnvironmentVariable('FFMPEG_DIR', $ffmpeg, 'Machine')
[Environment]::SetEnvironmentVariable(
    'PATH',
    [Environment]::GetEnvironmentVariable('PATH', 'Machine') + ";$ffmpeg\bin",
    'Machine')

# The FFmpeg bindings are generated during the build by bindgen, which loads
# libclang. Only that library is wanted, so the installer is unpacked rather
# than run: nothing is registered and nothing lands on PATH.
Start-Step 'Unpacking libclang'
Get-Verified -Url $settings.llvm_url `
             -Sha256 $settings.llvm_sha256 `
             -Path "$root\downloads\llvm.msi"
$unpack = Start-Process -FilePath 'msiexec.exe' `
                       -ArgumentList '/a', "$root\downloads\llvm.msi", '/qn', "TARGETDIR=$root\llvm" `
                       -Wait -PassThru
if ($unpack.ExitCode -ne 0) {
    throw "unpacking LLVM exited with $($unpack.ExitCode)"
}
$libclang = Get-ChildItem -Path "$root\llvm" -Recurse -Filter 'libclang.dll' | Select-Object -First 1
if (-not $libclang) { throw 'the LLVM package holds no libclang.dll' }
[Environment]::SetEnvironmentVariable('LIBCLANG_PATH', $libclang.DirectoryName, 'Machine')

# The repository's recipes are bash scripts, so `just` on this machine is
# useless without a shell to run them in. Git for Windows carries one, and the
# checkout the runner performs wants git anyway.
Start-Step 'Installing Git for Windows'
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
New-Item -Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock' -Force | Out-Null
New-ItemProperty -Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock' `
    -Name 'AllowDevelopmentWithoutDevLicense' -Value 1 -PropertyType DWord -Force | Out-Null
& 'C:\Program Files\Git\cmd\git.exe' config --system core.symlinks true
if ($LASTEXITCODE -ne 0) { throw 'could not enable Git symlink checkout' }

Start-Step 'Installing the Rust toolchain'
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

. 'E:\cache.ps1'
foreach ($tool in $settings.cargo_tools.PSObject.Properties) {
    Start-Step "Installing $($tool.Name) $($tool.Value)"
    cargo install --locked --version $tool.Value $tool.Name
    if ($LASTEXITCODE -ne 0) { throw "cargo install $($tool.Name) failed" }
}
& kache daemon stop
if ($LASTEXITCODE -ne 0) { throw 'could not drain compiler cache uploads' }

Start-Step 'Installing the GitHub Actions runner'
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
#
# First it tells the host when the evaluation licence ends, so the host can
# build a new guest before it does. It says so at every sign-in, because the
# file the port writes into starts empty whenever the guest is powered on.
$runner = @'
$licence = Get-CimInstance -ClassName SoftwareLicensingProduct `
    -Filter "ApplicationID = '55c92734-d682-4d71-983e-d6ec3f16059f' AND PartialProductKey IS NOT NULL" |
    Select-Object -First 1
if ($licence) {
    $expires = [DateTimeOffset]::UtcNow.AddMinutes($licence.GracePeriodRemaining).ToUnixTimeSeconds()
    $port = New-Object System.IO.Ports.SerialPort 'COM1', 115200
    $port.Open()
    try {
        $port.WriteLine("kithara-guest: licence-expires $expires")
    } finally {
        $port.Close()
    }
}

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

# The host may power the guest off as soon as it reads `done`, so everything
# written here is on disk first.
Write-VolumeCache -DriveLetter C
Send-Host 'kithara-guest: done'

# The sign-in that ran this one was granted by the answer file; every later one
# is the automatic sign-in, which only takes effect on a restart.
Restart-Computer -Force
