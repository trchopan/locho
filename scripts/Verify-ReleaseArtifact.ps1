[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Archive,

    [Parameter(Mandatory = $true)]
    [string] $ExtractionDirectory
)

$ErrorActionPreference = "Stop"

$checksum = "$Archive.sha256"
$resolvedExtractionDirectory = [IO.Path]::GetFullPath($ExtractionDirectory)
$currentDirectory = [IO.Path]::GetFullPath((Get-Location).Path)
if ([string]::IsNullOrWhiteSpace($ExtractionDirectory) -or
    $resolvedExtractionDirectory -eq [IO.Path]::GetPathRoot($resolvedExtractionDirectory) -or
    $resolvedExtractionDirectory -eq $currentDirectory -or
    $resolvedExtractionDirectory -eq [IO.Directory]::GetParent($currentDirectory).FullName) {
    throw "Refusing unsafe extraction directory: $ExtractionDirectory"
}
if (-not (Test-Path -LiteralPath $Archive -PathType Leaf)) {
    throw "Archive does not exist: $Archive"
}
if (-not (Test-Path -LiteralPath $checksum -PathType Leaf)) {
    throw "Checksum does not exist: $checksum"
}

$expected = (Get-Content -LiteralPath $checksum -Raw).Trim() -split '\s+' | Select-Object -First 1
$actual = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($expected.ToLowerInvariant() -ne $actual) {
    throw "Checksum mismatch for $Archive"
}

$archiveName = [IO.Path]::GetFileNameWithoutExtension(
    [IO.Path]::GetFileNameWithoutExtension($Archive)
)
if (Test-Path -LiteralPath $ExtractionDirectory) {
    Remove-Item -LiteralPath $ExtractionDirectory -Recurse -Force
}
New-Item -ItemType Directory -Path $ExtractionDirectory | Out-Null
tar -xf $Archive -C $ExtractionDirectory
if ($LASTEXITCODE -ne 0) {
    throw "Could not extract $Archive"
}

$nestedArtifactDirectory = Join-Path $ExtractionDirectory $archiveName
$artifactDirectory = if (Test-Path -LiteralPath $nestedArtifactDirectory -PathType Container) {
    $nestedArtifactDirectory
} else {
    $ExtractionDirectory
}
$binary = Join-Path $artifactDirectory "locho.exe"
foreach ($requiredFile in @("README.md", "CHANGELOG.md", "LICENSE")) {
    $path = Join-Path $artifactDirectory $requiredFile
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Archive is missing $requiredFile"
    }
}

if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "Archive binary is missing: $binary"
}
& $binary --help | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "Packaged binary failed --help"
}
Write-Output $binary
