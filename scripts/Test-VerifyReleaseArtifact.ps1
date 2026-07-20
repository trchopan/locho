$ErrorActionPreference = "Stop"

$scriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$verifier = Join-Path $scriptDirectory "Verify-ReleaseArtifact.ps1"
$fixtureDirectory = Join-Path ([IO.Path]::GetTempPath()) "locho-artifact-test-$([Guid]::NewGuid())"
New-Item -ItemType Directory -Path $fixtureDirectory | Out-Null

try {
    $packageDirectory = Join-Path $fixtureDirectory "package"
    New-Item -ItemType Directory -Path $packageDirectory | Out-Null
    $sourceBinary = $env:LOCHO_TEST_BINARY
    if ([string]::IsNullOrWhiteSpace($sourceBinary) -or
        -not (Test-Path -LiteralPath $sourceBinary -PathType Leaf)) {
        throw "LOCHO_TEST_BINARY must point to the extracted packaged binary"
    }
    Copy-Item -LiteralPath $sourceBinary -Destination (Join-Path $packageDirectory "locho.exe")
    foreach ($name in @("README.md", "CHANGELOG.md", "LICENSE")) {
        Set-Content -LiteralPath (Join-Path $packageDirectory $name) -Value "fixture"
    }
    $archive = Join-Path $fixtureDirectory "package.zip"
    Push-Location $packageDirectory
    try {
        Compress-Archive -Path * -DestinationPath $archive
    } finally {
        Pop-Location
    }
    $checksum = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$archive.sha256" -Value "$checksum *package.zip"

    $binary = & $verifier -Archive $archive -ExtractionDirectory (Join-Path $fixtureDirectory "extracted")
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "root-level ZIP archive was not verified"
    }

    try {
        & $verifier -Archive $archive -ExtractionDirectory "C:\" 2>$null
        throw "unsafe extraction directory was accepted"
    } catch {
        if ($_.Exception.Message -like "unsafe extraction directory was accepted") {
            throw
        }
    }

    Set-Content -LiteralPath "$archive.sha256" -Value ("0" * 64)
    try {
        & $verifier -Archive $archive -ExtractionDirectory (Join-Path $fixtureDirectory "bad-checksum") 2>$null
        throw "corrupt checksum was accepted"
    } catch {
        if ($_.Exception.Message -like "corrupt checksum was accepted") {
            throw
        }
    }

    Write-Output "artifact verifier negative tests passed"
} finally {
    Remove-Item -LiteralPath $fixtureDirectory -Recurse -Force -ErrorAction SilentlyContinue
}
